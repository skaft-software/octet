# octet TUI architecture

**Status:** Current implementation contract.

The interactive frontend owns terminal setup/restoration and presentation only;
`Agent` remains the sole model/tool runtime. The companion
[presentation contract](octet-presentation.md) defines the visual hierarchy,
approval, and terminal-outcome semantics shared by these mechanics. The
[ordinary command and picker surface contract](octet-command-picker-surfaces.md)
defines the transient discovery, selection, read-only report, status, and action
vocabulary that uses that hierarchy without adding a second TUI.

## Terminal guarantees

- The interactive frontend renders on the primary screen. `auto`, `terminal`,
  and `off` use the complete logical-frame renderer: the first frame writes
  every materialized row, pure appends flow naturally into terminal scrollback,
  and a width/height change or mutation above the previous viewport clears the
  screen and saved lines before replaying the complete frame. PageUp transfers
  rendering to the bounded, application-owned semantic viewport for the rest of
  that shell. Explicit `--mouse app` selects that viewport from startup.
- Auto, Light, and Dark use the compiled default layout. Auto adapts to the
  detected terminal background; Light and Dark explicitly select contrast. All
  retain model-aware accents and semantic status colours. Named file loading
  follows the bounded [theme loader](../themes.md).
- Raw mode, bracketed paste, keyboard enhancements, and mouse reporting are
  enabled only when supported and restored idempotently. Every
  interactive frame is bracketed by CSI 2026 synchronized-output markers;
  terminals that do not implement the private mode ignore it, while octet's
  backend still uses the markers to batch each frame into one flush. octet's
  composer uses a positioned hardware cursor; every renderer construction
  explicitly keeps that cursor visible.
- Mouse reporting is disabled by default, preserving native drag selection and
  wheel scrolling. `--mouse app` enables capture for semantic wheel navigation
  and selection, but keyboard viewport ownership does not depend on capture.
  Portable terminal protocols do not report a user's native scrollback offset,
  so uncaptured wheel history remains terminal-owned.
- Redirected, unknown, or explicitly plain terminals use the chronological
  fallback without cursor-control sequences.
- Provider, tool, and user text is sanitized before terminal output.
- Rendering never relies on color alone; no-color and ANSI-16 paths preserve
  structure.
- The generic renderer crate enforces `#![forbid(unsafe_code)]`; OS-specific
  terminal setup remains isolated in octet's terminal boundary.
- Raw ANSI diagnostics are disabled by default. `OCTET_TUI_WRITE_LOG` enables an
  exact backend-byte capture to an explicit file or a unique file in an existing
  directory; these traces are sensitive because they include displayed content.

## Startup identity

Routine startup phases are silent, including session lookup, replay, and fork
preparation on fresh, continued, resumed, and forked launches. Saved-session I/O
uses the silent lifecycle input owner; a fresh launch resolves configuration
inline without cloning the full config or dispatching a replay worker. The
composer remains editable until the atomic ready frame installs history and
identity. Setup/selection panels and actionable errors keep their normal owners;
quiet startup does not suppress cancellation or shutdown diagnostics. Scoped
`--models` discovery and explicit session selection also use that input owner.
An existing `--session-id` keeps its saved model unless a model is explicitly
selected; scope order alone does not overwrite it.

The working directory appears only in the footer, not again in the splash.
The splash keeps its byte-aligned spacing, version, changelog/update hints,
permissions, and exit help. Its normal compiled-default mark occupies 16×4
cells with eight contiguous columns and the `01101111` silhouette. The footer,
not the splash, owns the current model/reasoning row. Full access is a
warning-class permission mode, not an ordinary failure.

The eight-bar byte mark retains its model-blended gradient and finite colour
sweep on true-colour terminals. ANSI256 and ANSI16 instead use one
background-balanced model accent uniformly across all bars, without brightening
animation. With no model accent, the theme's model accent is used. Explicit
custom splash colours keep precedence; no-colour output uses terminal-default
foreground. Custom-theme fallback geometry remains unchanged; short compiled
cards prioritize update and permission state over the changelog hint. The
terminal background remains unchanged. User-customized ANSI16 palettes can
still affect actual contrast.

Interactive startup performs one best-effort newer-stable-release check outside
the input and renderer loops, skipped in offline mode and cancelled on exit.
A newer release adds an accent hint immediately below the muted
`/changelog · what's new` row in the splash's right-hand version column:
`↑ v<VERSION> available · run` followed by `octet update` rendered as Markdown
inline code. Both hints use bounded compact/ASCII fallbacks; neither occupies a
full-width footer beneath the logo. If conversation history has already frozen
that prefix, the result becomes one UI-only rich live-tail notice instead of
forcing historical replay. Its action retains inline-code styling, while
semantic copy contains plain command text. Ordinary notices remain literal.
No update is installed automatically. The bundled current-version changelog
uses the same rich Markdown renderer in a scrollable report.

## Transcript and input

The transcript is semantic blocks rather than a terminal framebuffer. Wrapped
layouts are cached per block and width, and streaming invalidates only changed
blocks, but the root component presents the complete materialized logical frame
to `sexy-tui-rs` on every interactive render. Removing a transient tail
status truncates only that block's cached rows and metadata when rendered, or
keeps the existing cache prefix when the status has not received a frame, so
admitting the next tool does not reconstruct historical Markdown while holding
the shared shell state. The terminal renderer retains
`previousLines`, logical cursor, hardware cursor, maximum working height, and
previous viewport-top state. It finds the first and last changed physical rows,
repaints only that range when it remains addressable, and uses bottom-row CRLF
appends to let new rows enter native scrollback.

An ordinary trailing pending tool now has a height-bounded **live preview**:
its intent and newest progress remain on the addressable screen, with an explicit
`result pending` omission marker when space permits. This projection does not
truncate the semantic/source cache. On completion it is replaced by the canonical
result under the normal disclosure policy, which can then enter saved history
exactly once. Octet disables shrink-triggered clearing for its renderer; the generic
renderer retains its own shrink policy. Optional dot/shimmer/timer ticks do not invalidate
headings already above the native live-screen seam.

An active roster never clips subsequent unrelated conversation: only an ordinary
trailing pending tool may use the bounded preview, while genuine historical
roster updates can still require full replay.

The current deterministic shell/renderer/VT matrix also tests fragmented table
rows through narrow/wide width and height changes, exactly one required replay
per resize, and late-reference finalization with exactly one historical repair.
Repeated no-change frames remain quiet; source/copy and exactly-once history
sentinels remain authoritative. These tests model emitted VT and saved-line
reset, not a physical emulator's reflow, paint, or native selection.

This covers the tested pending → progress → result/error case, not every historical update.
Real updates to historical concurrent tools or aggregate run outcomes,
retrospective Markdown changes, resize, and other structural transitions can
still take the replay path below. Worker-roster metric updates revise only the
mutable orchestration tail, not earlier parent rows; moving that tail behind new
parent output can still require a full replay above the native viewport. Do not
suppress that path without an emitted-history policy that preserves real results. Maintainer-reported Terminal.app, Ghostty and
Ghostty → SSH acceptance preceded 0.7.4 publication, but a subsequent model-switch
regression showed stale splash
rows. Acceptance of one journey does not qualify every reader/selection path.

A change above the old viewport cannot be repaired with cursor addressing.
That path emits `ED 2`, homes, clears saved lines with `ED 3`, and
replays the complete materialized frame. Width changes do the same because line
wrapping changed; height changes do so outside Termux. Disclosure contraction,
theme repaint, overlays, and dynamic composer chrome therefore cannot leave an
unwritten semantic gap in terminal history: they either take the visible-row
differential path or the authoritative full replay path. Kitty image placements
participate in the same changed-range expansion, targeted deletion, reserved-row
painting, and full-replay fallback.

Default terminal-owned resume materializes the complete active branch before it
is rendered, because terminal scrollback cannot prepend a deferred prefix later.
Explicit application-owned mode may retain the bounded tail-first hydration
optimization: semantic PageUp, selection, or copy can materialize older rows in
that mode without claiming they already exist in native history.
Application-owned mode—selected at startup by `--mouse app` or claimed by
PageUp—uses `follow_tail` plus a monotonic transcript commit ID, semantic copy
text offset/affinity, visual fallback, and desired screen row to select one
bounded viewport. `scroll_from_bottom` remains only the cheap navigation delta;
the semantic anchor rebases it after growth, contraction, deferred-history
prepends, and width/height changes. Scrolling above the tail keeps semantic rows
fixed while one Markdown block continues to grow, increments the new-output
state, and exposes the PageDown return-to-live affordance.

Terminal-owned modes preserve native selection and ordinary append scrollback,
but octet cannot observe or freeze a reader's position. A full replay replaces
the application's saved-line presentation and therefore returns the terminal to
the live frame. Semantic copy retains stable coordinates in either renderer;
application-owned drag selection is available only while mouse capture is
enabled. Terminal-owned resume eagerly loads the complete active branch;
application-owned resume loads a bounded tail and materializes older blocks when
semantic navigation or selection reaches them.

Held-key repeats are accepted only for text editing and navigation. One-shot
actions such as submit, panel confirmation, close, abort, and reasoning/summary expansion
require a fresh key press.

The composer supports multiline editing, bracketed paste, large-paste chips,
media attachments, dropped paths, gitignore-aware `@` completion, and Tab
completion for relative, parent, home-relative, and absolute path tokens.
Up/Down selects from a visible path or mention menu, whose bounded window follows
the selection; Tab inserts that result. With no visible menu or a cursor inside
the draft, arrows retain visual editor navigation.
Backspace inside or immediately after a registered paste/attachment chip removes
the whole mask and its ledger association. Ordinary bracketed text is not a chip;
neighboring Unicode text and other attachments remain unchanged.
Slash-command discovery, file mentions, and filesystem completion render inline
directly below the composer. While matches are visible, the suggestion surface
temporarily replaces the model and token status row; the status returns as soon
as completion closes. Matches use compact rows with action hints in a footer;
the active match and hint keys use the active model's adaptive accent, matching
picker focus and the composer without treating the focused item as provider
provenance.
Executable-extension status/header/footer contributions never occupy that row.
The default footer groups model, reasoning, context percentage/limit and cumulative
session cost on the left with semantic dot separators (ASCII fallback supported),
and right-aligns the workspace path. It abbreviates home to `~` and shortens or
omits the path first on narrow terminals. Active-run identity, estimated-context
markers, unavailable pricing and uncertain-cost subtotals retain their authority.
Read-only `/help`, `/status`, `/context`, `/cost`, and `/cache` reports use the
same title/purpose/status/footer vocabulary as ordinary pickers. They occupy a
temporary viewport surface rather than transcript history, start at their first
semantic body row, and support Up/Down, PageUp/PageDown, Home, and End scrolling;
Escape or Left returns to the composer.
Generic presentation snapshots do not create persistent chrome. First-party
subagent activity occupies one mutable tool-like **Subagents** transcript block,
including between root turns. Its bold heading counts worker states and points
to `/subagents`; up to four active child lines show task and `↑input ↓output`
tokens. Input includes uncached, cache-read, and cache-write usage; streamed
output estimates carry `~` until provider usage settles. Later parent output
is placed above this tail, and settlement fixes the summary in place without
per-worker notices. Ctrl+O retains disclosure; `/subagents` exposes the
complete roster, exact outcomes/reasons, model, tools, cost, and read-only child
transcripts. Neither estimates nor UI refresh change billed usage or budgets.
Raw first-party orchestration calls/results, including errors, stay out of the
interactive transcript and semantic copy, live and on replay. This does not
change model-visible errors, durable results, accounting, ordinary tool/run
failures, or approval prompts. Native `DelegationUpdated` events feed the view;
no slash-command polling is needed. [Presentation contract](octet-presentation.md#information-layers).

Live child cost is added to the host-owned cumulative footer only until root
settlement persists matching `delegated_agent` usage records; idle rendering
therefore cannot count it twice.

Accepted steering waiting for the next model boundary and local queued
follow-ups share a compact pending-state hint above the composer. It is capped
at two rows: a count and one clipped preview, with a `+N more` suffix. Admitted
steering takes preview priority because it is delivered before local follow-ups.
Explicit newlines receive a visible marker; the preview never expands into a
second transcript. The recall affordance is advertised only while at least one
queued entry is genuinely editable, so the hint never promises a recall that the
agent-side claim would refuse.

`/extensions` opens an interactive installed-bundle activation panel instead.
The no-argument `/subagents` command supplied by `octet-subagents` opens a
frontend-owned worker list; Up/Down moves focus, Enter opens the selected bounded
read-only transcript, and Escape or Left returns from the transcript to the
list. While open, the same owner-bound status command reconciles the host's
authoritative worker state and publishes complete presentation revisions; the
frontend preserves focus by stable node ID and revalidates the latest typed
session reference immediately before opening. Transcript panels start at the
live tail and support arrow, PageUp/PageDown, Home, and End scrolling.
Directory completions retain their trailing separator so completion can continue
one level at a time, and whitespace in completed names is backslash-escaped.
Media is capability-gated at attachment time and remains ordered with text when
submitted. PDFs are not decoded or sent as multimodal
payloads: a dropped PDF receives a composer chip, but submission resolves that
chip to the file path as text so the model can inspect it with file tools.

The compiled default composer, transcript, and overlays share one horizontal
grid. Full-width rules and cards begin at terminal column 0, as do event and
prompt markers; primary text begins at column 2 and nested detail at column 4.
The composer leaves the terminal canvas unfilled and uses stable top and bottom
rules in the exact model-lab colour selected for the next prompt. Starting work
or changing draft content never recolours or animates them. Content rows carry
no side borders, so copied prompt text cannot include frame characters. Composer
height starts at one row and scales proportionally within a terminal-height cap.

Each submitted prompt persists its model-lab colour. The compiled default
uses it for the marker and content-hugging highlights on each wrapped text line,
including attachment labels; inline Markdown emphasis, code and links retain
their own styling within the highlight. The gutter, blank spacing, and
trailing cells keep the terminal canvas. Switching models cannot recolour historical prompts: it
only recolours the composer and future prompts. Custom cards/rails retain their
configured full-cell treatment. Fenced Markdown
code is borderless and uses the compiled default's terminal-adaptive shading.
Default tool/shell surfaces reserve a two-cell right gutter, reduced in tiny
panes, before wrapping headers and nested output. Their source and copy text are
unchanged; custom surfaces retain their configured geometry.
The unknown-profile fallback remains unpainted. Assistant prose and submitted
prompts use the available width after the gutter, including list/quote
indentation; technical blocks retain viewport width. Static and streaming
layouts share the same policy.

## Streaming presentation stability

Newlines are interpretation boundaries, not a blanket visibility gate. Ordinary
prose stays visible immediately. Ambiguous fence/info strings, closing fences,
structural delimiter lines, and incomplete table rows wait for classification;
raw input stays exact and semantic copy retains the withheld decoded text.
Completed inline code can disambiguate a possible backtick fence without LF.

Parser scheduling does not grant permission to replace a rich preview with raw
Markdown. Geometric discovery passes do not briefly collapse literal source
lines into soft-wrapped paragraphs. A rich paragraph exceeding its inline parse
budget keeps its interpreted prefix and appends a literal continuation until a
proven block boundary or canonical finalization. The compiled default sizes code
surfaces and table columns from the viewport, so a later longer cell/line does
not reshape earlier rows. Static and streaming rendering use the same geometry.

Complete source lines, stable semantic blocks, unchanged visual prefixes, and
native-history commits remain different boundaries. Finalization still produces
canonical Markdown; it can require a retrospective repaint. Very large tables
can fall back to literal complete rows beyond the mutable parser budget. These
policies reduce live churn, not establish that every Markdown boundary or native
reader journey is qualified. See [the performance contract](performance.md).

## Approval panels

Confirmation panels retain the request title, two immutable actions, and one
shared bounded consequence detail. The detail is rendered once, terminal-safe,
and capped at three rows. Short terminals reserve space for the selected action
before detail: when no action row is visible, Enter is ignored and the panel
stays open. Confirmation panels have no filter input or synthetic item count, so
arbitrary text cannot alter the decision set.

## Reasoning presentation

Every accepted run opens a `Working` row immediately. After the provider emits
an actual reasoning delta, collapsed reasoning uses a bold `Thinking` activity
label with a quieter model-adaptive shimmer than `Working`. A real semantic ATX
or standalone-bold heading occupies the aligned detail row with a subdued
`Ctrl+O` hint. Without a heading, the compiled default puts the hint on the
activity row when it fits, retaining a detail row on narrow terminals. Ordinary
body prose is never inferred as a label, and provider text is sanitized before
display. The shimmer uses a monotonic 80 ms clock on the renderer thread,
independent of semantic-event frequency. Late frames select the current phase
without replaying missed frames; coalescing has a fixed deadline so incoming
notifications cannot postpone painting indefinitely. Animation changes style
rather than text or geometry and invalidates only the active status block.
The `Working` and `Thinking` labels share a foreground-only moving sweep, with
`Thinking` travelling a shallower luminance range; activity and reasoning dots
keep a solid glyph while their foreground pulses with the label. The known
Dark/Light TrueColor and ANSI256 physical field parks briefly after crossing
the label. Elapsed and countdown text update independently; grapheme clusters
stay intact. ANSI16, unknown-background, reduced-motion, and no-color paths
retain their compatibility/static behavior.
Before any model delta, an opted-in endpoint readiness update may temporarily
replace `Working` with `Provider queued`, `Loading Provider`, or `Provider
ready`, plus bounded sanitized detail. It reuses the active status block rather
than creating reasoning or transcript content. The first real model delta,
retry, compaction, and authoritative terminal outcome remove that endpoint
label; a completed provider turn restores generic `Working` while the run stays
active.

Visible assistant text does not prove that the owning run has settled. Exactly
one trailing `Working (<elapsed> • esc to interrupt)` row remains while the run
is active, including after public text; its timer is measured from the latest
non-steering user prompt and refreshes every second. Tool admission replaces
that row with the tool lifecycle, and authoritative run settlement removes the
final activity row. Compaction uses `Compacting context`, while tool execution
retains its tool-specific lifecycle row. Expanded reasoning keeps the same grid
without an event-margin dot or a synthetic first-row bullet.

## Run outcomes

Both normal completion and completion with warnings use the success glyph
and `completed`; per-call failures remain in their own tool blocks.
An interruption remains warning-class; it is not painted as success or failure.

A failed run keeps the compact `failed · <duration>` lifecycle row and follows it
with the actionable error reason even while other disclosure remains collapsed.
The reason is credential-redacted at the inference request boundary, then
terminal-sanitized and capped at 4 KiB on a UTF-8 boundary by the TUI. It is
included in semantic transcript copy so it can be reported without recovering
raw provider envelopes or headers.

## Tool presentation

Tool calls expose deterministic intent and lifecycle rows. Event-margin dots
identify active collapsed reasoning, assistant responses, and tool or shell
execution, and every dot uses the same glyph footprint. The collapsed-reasoning
and activity dots keep a solid, fixed-size glyph whose foreground pulses with
the activity-label sweep. `Working` and `Thinking` shimmer in the foreground
where supported; reduced-motion and no-color paths remain static.
Assistant-response dots remain steady; active tool and shell dots may pulse
through foreground and muted tones rather than changing size.
Successful completed event dots use green,
and failed tools use red. Raw protocol arguments and envelopes, unsanitized
failure evidence, and extension-rendered payloads remain internal
accountability/provenance data
and are excluded from transcript copy. For operational feedback, the TUI renders
bounded sanitized projections: search results use a muted tail, while retained
Bash/local-shell output uses a secondary readable foreground and edit/write
results use a bounded unified diff. A collapsed
failure retains one bounded actionable reason or explicit hidden-output count;
complete captured failure output stays available through global disclosure.
Omission metadata distinguishes a collapsible UI tail from bytes already
discarded by the tool capture.

Tool values begin two cells after their labels (with a six-cell minimum for
short names), avoiding a wide dead column while keeping each wrapped header's
value column fixed. A muted vertical `│` joins
each wrapped header row to the single `└` that begins its nested output, making
the output's ownership visible without adding another indentation level.

Terse compiled-default Bash headers retain two command-preview rows, wrapping
at whitespace where possible and hard-wrapping oversized graphemes by terminal
cell width; file themes retain their prior three-row limit. An exact hidden-row
count follows. The full command remains available through disclosure and semantic
copy. Retained output uses concise UI-collapse counts distinct from irrecoverable
capture-byte loss. The
command preview is independent of the output-tail budget.

Ctrl+O toggles the global disclosure mode for retained reasoning, compaction,
search output, Bash commands, Bash/local-shell output, and edit/write diffs.
`/verbose [on|off]` controls the same mode. Expansion cannot recover capture bytes that the tool
already discarded.

Final structured tool results remain provider-visible and persisted when the
agent protocol requires them to continue a tool turn. This is operational
model context, not a TUI disclosure channel. Live `ToolProgress` is ephemeral
and is not persisted or sent to the model. Terminal-gate action receipts are
bounded accountability input to the gate checker only, not ordinary model
context.

## Sessions and resources

The session resume panel opens with current-workspace sessions and can lazily
switch to all workspace directories. It supports fuzzy, quoted-phrase, and
`re:` regular-expression filtering; recent, title, and message-count ordering;
named-only filtering; optional path details; pinned/fork/current markers;
recoverable trash; and in-place renaming. Cross-workspace rows are browseable
but cannot be resumed into a differently scoped live App. Responsive picker
geometry uses the same column-0 marker, column-2 primary text, and column-4
detail grid as the composer and transcript: narrow terminals use compact rows,
regular widths stack title and subdued metadata, and widths of at least 112
columns may return to side-by-side columns. Selection and active scope controls
use the active model's adaptive accent and preserve focus by semantic item index
across filtering and resize.

`/fork` opens a bounded active-branch user-message picker, including a
whole-conversation head row, and restores the selected prompt into the new
composer. `/clone` copies the active head without opening a picker. Both create
provenance metadata before the ordinary idle-boundary session rebuild.

The durable connector tree presents entry IDs and kinds deterministically (`sessions
inspect` on the command line; the withdrawn `/tree` overlay used the same renderer).
It marks every ancestor on the selected branch with `+`, the exact durable head
with `*`, and keeps abandoned forks visible. `/reload` recomposes AGENTS
instructions, rescans skills and prompts, and rebuilds the Agent only at an idle
boundary.

Model selection is available through a picker or direct `/model <id>`. Each
model-picker opening starts at the first selectable result; `(current)` remains
an annotation, not an instruction to scroll into the middle of the catalog.
Successful model/thinking changes update stable status without an extra transcript
success notice. Queued acknowledgements and failure diagnostics remain visible.
The picker groups providers alphabetically, then models alphabetically within each
provider. Provider names appear once as non-selectable headings; aligned model
metadata is limited to input/output price, context window, and vision/audio
support. Tool and reasoning support are omitted from these rows. Thinking
choices include only the active model's advertised `min_effort..=max_effort`
range.

The context composition bar is ordered left-to-right by semantic model-input
order: earliest framing at the left, chronological conversation and pending
adjustments in the middle, and output reserve/remaining capacity at the right.
Every displayed component owns a distinct colour, and a category already
accounted for by its actual owner is not duplicated as a decorative slice.

`/extensions` lists managed executable bundles only; the separately packaged
`octet-serve` application is not an activation target. Enter updates only the
selected name in the user config's `enabled_extensions`, never trust, then
rebuilds the Agent and extension host at the idle boundary so enable and disable
take effect immediately. A project or explicit definition shadowing the managed
global bundle is visible but not toggleable from this menu. If project,
environment, or command-line activation participates in the effective list, the
menu is read-only rather than claiming a user-config edit will survive the next
launch; project precedence is rechecked at action time. Enabled-but-unavailable
bundles remain disable-only, while source-changing trust, tool collisions, and
explicit required-tool removal fail closed.

## Presentation verification

Focused frame tests render the durable prompt, terminal outcome, composer, and
shared geometry at short (`46×8`), regular (`80×24`), and wide (`120×40`)
sizes, asserting that every row remains within the terminal width. Separate
regressions prove that an invisible approval action cannot be confirmed,
approval detail remains retained and rendered, and collapsed failures keep a
bounded actionable reason.

## Active-run controls

- Enter queues a local, typed follow-up. Normal completion arms exactly one FIFO
  dispatch through the idle prompt owner after the old Run and children settle.
  Session switches park local follow-ups by session path. Returning restores the
  drafts without carrying over dispatch authorization.
- Ctrl+S admits live steering to RunControl at the next model boundary. Its
  pending display is removed only by the durable delivery acknowledgement;
  undelivered steering is restored on settlement.
- Option+Up/Alt+Up recalls the newest editable pending message — an
  Enter-submitted follow-up or a Ctrl+S steering entry — into an empty composer,
  preserving attachment/paste payloads. Recall of steering is arbitrated by the
  submission's own receipt, not by the delayed delivery event: it succeeds only
  before the agent claims that input for persistence, and releases the reserved
  control budget when it wins. A refused recall leaves the entry queued for its
  FIFO delivery projection. Sticky `/answer` input is deliberately not
  retractable because it changes the run's tool policy. The chord neither
  interrupts nor submits, and never overwrites a draft.
- Escape closes the current panel/overlay/slash popup first. At the active
  composer it interrupts and arms queued dispatch only after authoritative
  aborted settlement; it never sends the unqueued draft. Repeated Escape while
  settling cannot arm dispatch after a plain cancellation.
- Ctrl+C first clears a nonempty draft; with an empty draft it interrupts without
  dispatch and is ignored while idle. An abort revokes any earlier Escape dispatch.
  Failure, max-turn limits, stream loss, and
  close do not automatically retry queued prompts; local entries remain editable.
- Auto-dispatched prompts use normal composition and persistence, but are never
  reinterpreted as delayed `!` shell commands. A failed submission restores its
  chips alongside any newer draft instead of overwriting that draft.
- Ctrl+D requests a coordinated close from every input owner, including
  pickers, tool prompts, lifecycle waits, and local shell commands. Active work
  is aborted and settled before the process exits.
- Safe presentation commands execute immediately.
- Model, reasoning, session, compaction, and reload work is queued in
  order and applied after the active `Run` releases its Agent borrow.

## Interrupted inference presentation

`RecoveredOutput` is displayed as a separate notice labelled as partial text or
reasoning from a previous interrupted attempt, not as this answer. It does not
feed current output deltas, token-speed counters, provider replay, or accounting.

`ProviderRetry` invalidates every model block owned by the unfinished attempt,
including reasoning already closed when public text began. Ownership uses stable
transcript identities, not a suffix boundary: independently arriving notices and
worker activity survive removal, with selection, active indices, and layout
caches rebased through the ordinary removal path. `TurnFinished` releases that
ownership at the accepted assistant boundary. Generated media is not inserted
provisionally by the interactive event consumer, so a rejected media payload
cannot enter disclosure or copy.

`ProviderWaitingForNetwork` updates that activity without rolling back output:
a definitely pre-send failure has no new provisional answer to discard. It shows
`Waiting for network` and an attempt count without inventing a finite denominator.

Retry activity is one typed, presentation-only record on the mutable `Working`
row. Repeated retries replace it rather than appending causes to the transcript.
Its countdown derives from the observed backoff; once the delay elapses it says
`Retrying`, not an invented provider deadline. While retry state is present, the interrupt hint
remains but the run-elapsed suffix is omitted, even after the countdown expires.
The countdown still uses the existing one-second refresh cadence.
`TurnStarted`, meaningful output,
compaction/tool transitions, cancellation admission, and settlement end that
backoff presentation. Raw causes remain with the event/diagnostic consumers;
print, plain, and RPC output retain their existing contracts. Removing rejected
output already above the native viewport can require saved-line clear and full
replay. Quiet notices do not promise an undisturbed native scrollback position.

API waiting is independently scheduled from animation: the real renderer thread
wakes for status shimmer at 80 ms, elapsed time at one-second boundaries, and
resize polling at 100 ms, without a busy frame loop. The gated-loopback PTY
regression (`real_octet_held_api_wait_pty_contract`) exercises held ordinary and
manual-compaction requests, actual ANSI style changes, keyboard echo and resize,
500 ms input/cancellation budgets, static no-color motion, and bounded idle and
active frame counts. The interactive driver fixtures separately cover delayed
success, timeout, transport failure, and cancellation. These deterministic
fixtures do not establish the cause of every reported freeze: expensive layout
still shares a shell mutex with input, and real terminal/live-provider and
long-duration soak qualification remain separate.
