# octet in Tern

[Tern](https://stencil.so/tern) is Stencil's native multiplexing terminal. It
does not only paint a character grid: a program can describe its UI as semantic
data and Tern draws it natively — typeset markdown and math, real split diffs,
meters, charts, checklists, images and pages. That is the **Tern Surface
Protocol** (TSP), and it is why `omp`, the reference client, looks like an
application rather than a terminal program.

`crates/octet-tern` is a TSP client for octet: wire types, APC framing, a tty
session with flow control, an octet-theme → Tern palette projector, and scene
builders for octet's coding surfaces. This document records the protocol as
observed and the RAIL mapping octet uses. RAIL implementation is authorized;
complete native parity and release qualification are not established. The
[S01–S37 qualification ledger](design/tern-rail-qualification.md) keeps every
existing octet surface family in scope, including the remaining gaps.

## Wire format

Every message is one APC string on the program's own tty:

```
ESC _ tsp ; <verb> [; k=v]* ; <body> ESC \
```

- **Verbs** program → terminal: `q` query, `o` open a surface, `f` a frame of
  ops, `b` a blob, `t` the theme palette, `x` close.
- **Verbs** terminal → program: `r` reply, `e` event.
- A body larger than the negotiated limit (default 65536 UTF-8 bytes) is chunked
  into pieces sharing `c=<id>`, all but the last carrying `m=1`.
- Tern advertises itself with `TERM_PROGRAM=tern`; a program may start
  optimistically against the full v1 vocabulary and refine from the `hello`
  reply (`{v,term,kinds,features,apc,credits,cols,cell,dark,reduceMotion}`).

### Messages

| verb | body |
| --- | --- |
| `q` | `{q:"hello",v:[1],app,ver}` or `{q:"blobs",ids}` |
| `o` | `{id,mode:"inline"\|"screen",title?,role?,adopt?}` |
| `t` | `{sf,dark?:{token:#rrggbb},light?:{…},name?:{dark?,light?}}` |
| `f` | `{sf,s,ops:[…]}` — `s` is a monotonic per-surface sequence |
| `x` | `{id,keep:bool}` |
| `b` | a base64 blob, with `id`/`mime` params |

### Ops

`add(id,parent,before,node)`, `set(id,props)`, `text(id,mode,text)`,
`splice(id,at,del,text)`, `move(id,parent,before)`, `del(id)`, `settle(id)`,
`focus(id|null)`, `reveal(id,where)`, `suspend`, `resume`.

Optional `scroll(id,by)` requires an actual `hello.features` advertisement of
`scroll`. Its directions are `line-up`, `line-down`, `page-up`, `page-down`,
`start` and `end`. No advertisement means no scroll op, not an optimistic
assumption. This bridge does not establish semantic search, selection, prompt
jumps, pointer geometry or reader-position parity.

### Node

`{id,k,p?,c?}` — a stable id, a [kind](#kinds), an optional prop bag and
optional children. A surface's root is the surface id; the first frame adds the
fixed regions `main`, `dock` and `layer` as columns under it.

### Kinds

`col row card section rule spacer text md code diff ansi math image kv table
tree badge kbd icon spinner shimmer elapsed progress rate list item tabs editor
input status seg overlay toast rows picker prefs tool checklist agent chart
meter effort`

Data-first kinds (`tool`, `checklist`, `picker`, `prefs`, `agent`, `chart`,
`meter`, `effort`) carry the data and Tern draws everything; this is what makes
the surfaces look native rather than ANSI.

### Flow control

Each surface allows `credits` unacknowledged frames (default 2). The terminal
returns `{"ev":"ack","sf","s"}` per frame; a sender waits when the window is
full. `client::TernClient` tracks this. Standalone clients may own raw stdin;
octet uses `connect_shared_input` and supplies replies from its single frontend
input owner. Failed writes do not advance the sequence or retained tree.

## Theme projection

Tern "wears" a program's theme: the `t` palette maps token names to `#rrggbb`,
and the terminal derives its chrome (surface fills, card tints, composer, chart
colours) from them. The interactive renderer resolves octet's validated theme
snapshot separately for dark and light appearances, then applies the active
model family. It does not re-read theme files on model changes. The standalone
demo uses `theme::from_toml`; the interactive adapter uses the compiled semantic
roles in `tui/view/tern_theme.rs`:

| octet | Tern palette token |
| --- | --- |
| runtime `model_accent` / `success` / `error` / `warning` | `accent` / `success` / `error` / `warning` |
| `muted` / `dim` / `foreground` | `muted` / `dim` / `text` |
| `border` / `border_idle` / `border_focused` | `border` / `borderMuted` / `borderAccent` |
| `user_msg_bg` / `user_msg_text` | `userMessageBg` / `userMessageText` |
| `assistant_msg_bg` / `assistant_msg_text` | `customMessageBg` / `customMessageText` |
| `tool_title` / `tool_output` | `toolTitle` / `toolOutput` |
| `diff_added` / `diff_removed` / `diff_context` | `toolDiffAdded` / `toolDiffRemoved` / `toolDiffContext` |
| `md_heading` … `md_list_bullet` | `mdHeading` … `mdListBullet` |
| `syntax_*` | `syntax*` |
| `[roles."surface.user"].background` | `userMessageBg` |
| `[roles."surface.tool"].background` | `toolPendingBg` / `toolSuccessBg` / `toolErrorBg` |
| `[roles."surface.shell"].background` | `statusLineBg` |
| `composer_bg` / `md_code_bg` | `pageBg` / `infoBg` |

Tokens left at the terminal default (`"default"`) are omitted. Reasoning and
status tokens octet does not define are derived from `accent`/`dim`/`error`.
`[variants.universal]`, `[variants.dark]` and `[variants.light]` overlay the base
so both appearances are sent and Tern picks by the terminal's background.

## Proof of concept

`crates/octet-tern/src/bin/octet_tern_demo.rs` opens an inline surface, sends
the Cards theme palette and renders a representative session. Captured from a
real Tern pane:

```console
cargo run -p octet-tern --bin octet-tern-demo -- --theme examples/themes/Cards.toml
```

The rendered session shows a reasoning-free transcript with typeset markdown
(code, `$t_r = 1.5$s`, a table, a quote), an `Edit` tool card with a native
split diff (`+9 −4`), a `Bash` card with output, the turn usage row, the clocked
working row (`8.6s 38.6 tok/s`), the composer with a model chip, effort glyph
and context meter, and a demo todo HUD. This historical scene is a fixture,
not the production RAIL layout or evidence of current input-route qualification.

## Integration

Inside Tern, octet's renderer thread owns one retained inline surface,
`octet.session`. It does **not** start an ANSI TUI underneath that surface.
`RenderModel` publishes immutable accepted source; `RenderOwner` materializes
streaming text outside the frontend lock. Stable transcript commit IDs survive
prepends, streaming and model changes. The reconciler patches changed props
and adds/removes/reorders only the affected nodes; appending a turn does not
move the preceding history.

- `main` owns the welcome mark and transcript.
- `dock` owns working status and the composer.
- `layer` owns native autocomplete, picker sheets and report overlays.

RAIL uses one responsive `96ch` reading measure for transcript, welcome and
live composer chrome. The character-relative bound scales with Tern's font
and shrinks in narrow panes. The composer is an integrated, borderless `col`
with a separating `rule`, native editor, model and effort controls, context
meter, session cost and Send/Stop; it is not an `omp.editor` card.

The welcome is a compact conversation-local wrapping row. It uploads immutable,
content-addressed PNG bytes from the canonical byte-mark rasterizer and requests
40×20 logical pixels. The eight contiguous, baseline-aligned bars keep the
`01101111` silhouette (positions 1 and 4 half-height) and 2:1 proportions;
model/theme-aware colours remain supported. Unsupported images retain the
textual byte mark. Octet-owned `octet.welcome.*` roles avoid OMP's private
welcome sizing and animation hooks.

Assistant replies are unboxed `col` reader prose with a direct, stable `md`
leaf, not a labelled assistant card. User prompts use `omp.user` native cards
for Tern's right-aligned bubble treatment. Assistant prose and thinking retain
the observed `omp.assistant` / `omp.thinking` hooks; composer controls retain
their observed hooks. Those `omp.*` roles are Tern implementation details,
not portable TSP guarantees or omp branding.

Tern still owns typography and its focus ring. Increase **Font size** in Tern's
settings (or use Cmd+= / Cmd+- on macOS) for a larger interface. TSP v1 has no
per-application font-scale setting; octet does not multiply logical sizes by
the display's pixel density.

Slash, path and extension completions are native lists anchored above the
composer. All matching candidates are sent; the eight-row native viewport,
not the candidate catalogue, is bounded. Paging uses that viewport during
native ownership. Slash activation follows the resolved selection-confirm
binding, and cursor-only edits preserve navigation and popup dismissal. Their
gestures are fenced by the draft revision. Model, theme,
thinking, session, fork and subagent pickers reuse the host's catalogues and
filtering, with panel-epoch and source/catalogue-content-fenced gesture IDs.
A refreshed ordinal cannot silently authorize a different session or worker. The model picker has real provider
scopes, context/price columns, a current-model indicator, and a public-facts
preview (limits, input modalities, cache pricing and supported capabilities).
Provider marks use seeded initials, the protocol's supported text mark rather
than fabricated logos. Thinking is a compact native disclosure-sized sheet
with descriptions and a current check, not a large empty browser. Session
selection exposes workspace, sorting, named/path filters, explicit transcript
search, rename and delete actions; the selected session previews its saved
metadata. Scope and filter chips wrap separately from the primary actions.
Narrow layouts request a below-list preview; Tern may hide that preview at its
smallest widths, but Resume remains visible. Rename and delete confirmation
still use the existing host-owned ANSI-content modal flow; native session
rename is a remaining adapter gap. Settings/help reports are native
modal content rather than migration `rows` nodes. Internally styled documents
and approval labels may use native `ansi` content; this never runs or repaints
an ANSI TUI. A user bubble is filled with the projected `userMessageBg` and carries
Markdown source. Assistant Markdown has no model-name card heading or enclosing
fill; streaming patches the same retained leaf. Blank-line tightening preserves
code-fence contents and does not change the authoritative source/copy text. Markdown reports (`/changelog`,
`/hotkeys`) carry their source, so Tern typesets their headings, tables and
code rather than showing flattened text. `/hotkeys` groups bound actions by
area as key/description tables and lists unbound ids last. Native pickers publish the
catalogue total, the search caret, program-computed match ranges and keyed
footer actions, so the query reads as hits in the list. Ordinary choices use compact sheets with filter, count, full selected
label/detail and owned controls. Consent is excluded from these sheets and
native positive pointer routing: its existing `lg` modal keeps one bounded ANSI
body without duplicated titles or controls. The host's source-action receipt is
published only after the exact frame acknowledgement, and is rejected after an
unpainted selection or panel change. This acknowledgement does **not** establish
native geometric visibility; consent visibility remains unqualified.

The frontend remains the sole stdin owner. It reassembles and routes TSP
replies, credit, appearance, disclosure, picker and editor gestures. Editor
gesture offsets are checked UTF-16 boundaries before conversion to UTF-8;
stale lengths and invalid ranges are rejected. The wire has no independent
source edit revision: same-length stale edits remain a qualification gap, not
something request epochs or length checks prove safe.
Pointer controls use resolved keybindings, including disabled bindings.
Identified protocol fragments, malformed messages and oversized assemblies
are consumed rather than replayed into a draft. Only an ambiguous opening
Escape prefix has a short input-latency timeout. Genuine bracketed paste and
Enter/Escape keys remain frontend input. Native text Edit events do not carry
paste intent and must not be treated as attachment/upload consent.

Ordinary tool/extension input owns an epoch-fenced temporary native editor, shared
with raw-key editing and bounded to 4096 UTF-8 bytes. Range replacements are
atomic and UTF-16-checked; overflow is explicit. Submit/cancel return through the
existing host owner without editing the parent's draft, caret or attachment
ledger. Sequential requests cannot reuse a previous request's controls.

Secrets remain **HOST-PRIVATE**: the native sheet contains the prompt and host
input instructions, never an editor, answer value, masked value, copy source or
native confirm route. Secret typing, host Enter submission and raw Esc cancellation
bypass ordinary selection-key normalization, including remapped/disabled picker
bindings. Native cancellation is request-fenced and returns raw Esc to that private
owner. No unsupported password property is invented. While temporary
input, a panel or remote UI owns focus, ordinary composer edits/actions are
readonly or suppressed. Remote editor/fullscreen placements project their
validated native ANSI content rather than leaving an active ordinary composer.

Bash/exec tools and local `!` commands show the FULL command immediately in
wrapping `code` rails, without line numbers or a collapsible command target.
Captured output is absent from the native tree and transport until global Ctrl+O
requests verbose output; this gate includes command-image preparation, hashing
and blob upload, not just image nodes. Second Ctrl+O removes the output projection
without discarding captured source. A per-tool disclosure cannot bypass this gate.
`!!` retains its existing excluded-history semantics.

The command rail retains truthful lifecycle status, available duration and local
exit code; captured failure output also requires disclosure. Verbose Bash/exec
output patches one stable native text leaf as chunks arrive, including partial
lines, without a streaming cursor or changing to a diff widget for diff-like
command output. Non-command tools preserve deterministic display labels,
progress, file/diff projections and grouped-child disclosure.

Tool images respect the existing opt-in image preference. Validated bounded
payloads are hashed and uploaded once per content address; retained frames
contain only blob references, dimensions and safe descriptions. Unsupported
image kinds and rejected/disabled payloads retain text placeholders.

`/context` uses its captured semantic quantities for a stacked meter and a
token/percentage table, falling back to native key/value rows if tables are
unsupported. Reasoning uses real semantic Markdown headings and the host's global/per-trace
expansion state. Empty activity does not create a Thinking trace; Working remains
until real reasoning exists. Unknown reasoning duration is absent, not zero;
a settled trace does not prove a successful run. Compaction follows its own
expansion/global disclosure state. Working projects actual provider
queued/loading/ready, waiting and retry/backoff labels; retries omit run-elapsed
metadata and derive countdowns only from observed backoff.

Subagents occupy one conversation-local, commit-scoped mutable block, not a
pinned dock roster. It shows state counts and up to four host-supplied live
`↑input ↓output` token lines (provisional output marked `~`) plus overflow and
the existing stop-all hint. Settlement removes child rows in the same block;
hydrated activity without telemetry remains neutral evidence, not completed
workers. Detailed models, costs, tools, prompts, reasons and read-only child
transcripts stay in the inspector. No retry, context, request statistics,
percentages or measurements are invented when the host does not supply them.

A hidden pane suspends presentation without failing the renderer: credit
starvation and rejected background frames while hidden are not timeouts, and
returning to the tab forces a frame and re-asserts native keyboard focus.
An OS focus return that arrives with no TSP `Visible` event (another app or
overlay was in front, e.g. screen recording) takes the same recovery path via
the frontend's `FocusGained` signal: the next frame is forced, `composer.editor`
focus is re-asserted, and the draft is refreshed.
Resize, zoom and appearance events preserve native ownership and retained
identity. Explicit eviction reopens the surface and replays its regions. Credit
exhaustion coalesces changes until acknowledgements arrive; negotiation,
unsupported required kinds, protocol errors and acknowledgement timeouts
produce an explicit notice before handing ownership to the ANSI renderer.
Normal exit keeps native transcript history; suspension removes the active
surface and a resumed renderer negotiates a fresh one.

Detection uses `TERM_PROGRAM=tern`, and the policy is a first-class setting:
`--tern auto|on|off` (config key `tern`, env `OCTET_TERN`, older spelling
`OCTET_TUI_TERN`). `auto` is the default and negotiates native surfaces only
where the terminal advertises itself; `on` forces negotiation on any terminal,
which is how the protocol path is exercised outside Tern; `off` disables the
native backend outright and always renders ANSI, even inside a Tern pane. The
resolved policy is published once before the frontend starts, so the renderer
thread and the shared input filter always make the same decision. Outside Tern
with the default policy, the existing ANSI renderer is unchanged.

## Verification without desktop controls

```console
cargo test -p octet-tern --locked
cargo test -p octet-coding-agent --lib --locked tui::view::tern
cargo test -p octet-coding-agent --test tern_native_pty --locked
```

The PTY lane uses the real octet binary and input parser, a synthetic TSP
terminal, isolated HOME/workspace, and an inert loopback-only provider record.
It exercises paste, Unicode, fragmented replies, Escape, slash completion,
settings and theme selection without GUI automation or live provider calls.
Protocol/tree fixtures cover retained streaming, credit, eviction, appearance,
prompt provenance, command-output/blob disclosure and acknowledgement-gated
approval consent. Test commands are qualification entrypoints, not a claim that
they passed on this changing candidate. Fixtures are not actual provider,
authentication, consent or worker executions; synthetic PTY checks do not
establish a particular Tern version's pixels or complete interactions. See the
[qualification ledger](design/tern-rail-qualification.md) for remaining gates.

## Limits

TSP v1 has no arbitrary CSS or per-node RGB card styles. Tern owns fonts,
spacing, corner radii and base widget chrome; octet theme geometry and surface
recipes cannot be reproduced cell for cell. Octet supplies resolved dark/light
semantic colors, model accents, native structure and its own identity. ANSI16
and indexed colors expand through the standard xterm table; terminal-default
colors are omitted rather than guessed.

Because TSP v1 has no per-node colour, native prompt cards share the active
palette's user tint; the stored per-turn prompt colour still paints prompts in
the ANSI renderer. This is distinct from
running an ANSI TUI. Extension-defined styled rows also remain native ANSI
content until those extension contracts provide semantic nodes.

All 37 existing octet native surface families remain required, but native auth
instructions still lack an equivalent while authentication suspends the surface;
session rename is legacy ANSI content; native paste intent, revision-complete
edits, semantic navigation/search/selection and pointer geometry remain gaps.
Tern 0.3.1 is untested. This is an independent RAIL implementation deliverable,
not full-parity or release-ready qualification.

The native renderer does not claim omp's whole product surface: octet's current
settings command is a read-only report, not editable `prefs`; extension-owned
text is not reverse-parsed into fabricated checklists or charts. Transcript
subtrees are memoized and incrementally reconciled, but finalized history is
not yet evicted with `settle`.
