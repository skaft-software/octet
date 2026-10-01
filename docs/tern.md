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
full. `client::TernClient` tracks this and reads events in raw mode.

## Theme projection

Tern "wears" a program's theme: the `t` palette maps token names to `#rrggbb`,
and the terminal derives its chrome (surface fills, card tints, composer, chart
colours) from them. octet themes are projected via `theme::from_toml`:

| octet | Tern palette token |
| --- | --- |
| `accent` / `success` / `error` / `warning` | `accent` / `success` / `error` / `warning` |
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

The demo is standalone on purpose: a faithful TSP client plus a small theme
reader of its own, so the POC adds no visibility changes to
`octet-coding-agent`. Wiring octet's own TUI is a second
`sexy_tui_rs::Component` over the same semantic model — not a second stdout
path. The renderer thread already drives exactly one `Component` and one
`OctetTerminal`; a Tern renderer is selected in its place at construction.

Existing anchors (all crate-private today):

- **semantic model** — `hydrate::TranscriptItem`
  (`crates/octet-coding-agent/src/hydrate.rs:407`) is the canonical display
  projection (`User`/`Assistant`/`Reasoning`/`ToolCall`/`ToolActivityGroup`/
  `ToolResult`/`CompactionMarker`); `ShellState.transcript: Vec<TranscriptBlock>`
  (`tui/view.rs:1179`, the enum at `:358`) is what the renderer consumes.
- **frame seam** — `ShellComponent: sexy_tui_rs::Component`
  (`tui/view/renderer_runtime.rs:1287`).
- **role mapping** — `surface_roles(kind) -> (content, border, label)`
  (`tui/view/surface_layout.rs:74`) over the eight surface kinds, resolved
  through `OctetTheme::semantic_style(role) -> sexy_tui_rs::TextStyle`
  (`tui/theme.rs:663`) and `OctetTheme::glyph(name)` (`:644`).
- **composer/status** — `render_composer_surface`
  (`tui/composer_surface.rs:1014`), `render_ordinary_status`
  (`tui/view/ordinary_surface.rs:178`).
- **capability probe** — `sexy_tui_rs::CapabilityProbe::from_process()`
  (`crates/sexy-tui-rs/src/capabilities.rs:375`) exposes `term_program`.

Blockers (private today): `mod tui;` and `mod config;` are private in
`crates/octet-coding-agent/src/lib.rs`; every file-theme loader takes the private
`Config` (`tui/theme.rs:2145ff`; only `default_theme()` is config-free);
`TerminalBackground` and `ModelLab` are `pub(crate)`; `SEMANTIC_ROLE_VOCABULARY`
is `#[cfg(test)]`; `ShellState`, `TranscriptBlock`, `SurfacePlan` and
`ShellComponent` are `pub(crate)`/`pub(super)`.

Recommended path: add a curated `pub mod tern` facade **inside**
`octet-coding-agent` (which has private access) exposing a config-free theme
loader, a structured `SurfaceFrame` snapshot of the transcript/composer,
capability data, and the `Component` impl. `crates/octet-tern` then depends on
`octet_sdk` + `sexy-tui-rs` and consumes only that facade. Widening `tui`/
`config` to `pub` instead would expose far more surface than a TSP backend needs.

Once wired, the lifecycle is:

1. detect `client::is_tern()` (and keep the ANSI path otherwise),
2. on start, `open` an inline surface with role `octet.session` and send
   `OctetPalette::to_wire` for the loaded theme,
3. drive `scene` builders from octet's transcript/tool/composer model and send
   frames, re-sending `t` when the theme changes,
4. route terminal events (`resize`, `theme`, `toggle`, `select`, `action`,
   `edit`) back into the TUI.

## Limits

Tern owns the background, fonts and layout, so octet's `[metadata].terminal`,
font intent, `[surfaces]` geometry, `[layout]` and `[glyphs]` translate to
intent (card vs band, compact vs airy) rather than exact cells; glyphs are
replaced by Tern's native icons and box drawing. The `role` namespace is
program-defined, so octet uses `octet.*`.
