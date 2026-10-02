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
observed and the mapping octet uses.

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
and context meter, and the todo HUD in the dock.

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

The composer uses Tern's observed `omp.editor` layout with octet's native
editor, model and effort controls, context meter, session cost, and Send/Stop.
The welcome uploads octet's byte mark as immutable, content-addressed SVG;
unsupported images retain the textual byte mark. Assistant prose and thinking
use the observed `omp.assistant` / `omp.thinking` layout hooks. These `omp.*`
roles are Tern implementation details, not portable TSP guarantees or omp
branding. Tern still owns typography, geometry and its focus ring.

Slash, path and extension completions are bounded native lists anchored above
the composer. Their gestures are fenced by the draft revision. Model, theme,
thinking, session, fork and subagent pickers reuse the host's catalogues and
filtering, with panel-epoch-fenced gestures. The model picker has real provider
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
still use the existing host-owned modal flow. Settings/help reports are native
modal content rather than migration `rows` nodes. Internally styled documents
and approval labels may use native `ansi` content; this never runs or repaints
an ANSI TUI. A user prompt is a native `card` (tone `user`, Markdown body)
that Tern lays out against its own column, filled with the projected
`userMessageBg`. A follow-up assistant message is the same shape with the model
name as its `head`, so replies draw over the projected `customMessageBg` /
`customMessageText` / `customMessageLabel` palette rather than as bare Markdown
in Tern's default prose look; streaming still lives on the single `md` leaf, so
a growing reply patches one node. An ANSI wash padded to octet's PTY width came out ragged
wherever Tern's column was narrower. Markdown reports (`/changelog`,
`/hotkeys`) carry their source, so Tern typesets their headings, tables and
code rather than showing flattened text. `/hotkeys` groups bound actions by
area as key/description tables and lists unbound ids last. Native pickers publish the
catalogue total, the search caret, program-computed match ranges and keyed
footer actions, so the query reads as hits in the list. Approval consent is published only after the exact native frame
is acknowledged, and is rejected after an unpainted selection or panel change.

The frontend remains the sole stdin owner. It reassembles and routes TSP
replies, credit, appearance, disclosure, picker and editor gestures. Editor
gesture offsets are checked UTF-16 boundaries before conversion to UTF-8;
stale lengths and invalid ranges are rejected and the draft is re-synchronized.
Pointer controls use resolved keybindings, including disabled bindings.
Identified protocol fragments, malformed messages and oversized assemblies
are consumed rather than replayed into a draft. Only an ambiguous opening
Escape prefix has a short input-latency timeout. Genuine bracketed paste and
Enter/Escape keys remain frontend input.

Tool images respect the existing opt-in image preference. Validated bounded
payloads are hashed and uploaded once per content address; retained frames
contain only blob references, dimensions and safe descriptions. Unsupported
image kinds and rejected/disabled payloads retain text placeholders.

`/context` uses its captured semantic quantities for a stacked meter and a
token/percentage table, falling back to native key/value rows if tables are
unsupported. Subagent telemetry uses native `agent` nodes with authoritative
tools, tokens, priced cost, current tool, and terminal-clocked age; unsupported
agent kinds keep the existing compact activity text. No retry, context, or
request statistics are invented when the host does not supply them.

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
Protocol/tree tests cover retained streaming, credit, eviction, appearance,
prompt provenance and acknowledgement-gated approval consent. These tests do
not establish the pixel appearance of a particular Tern version.

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

The native renderer does not claim omp's whole product surface: octet's current
settings command is a read-only report, not editable `prefs`; extension-owned
text is not reverse-parsed into fabricated checklists or charts. Transcript
subtrees are memoized and incrementally reconciled, but finalized history is
not yet evicted with `settle`.
