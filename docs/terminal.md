# Terminal

[Documentation](README.md) · [Commands and keys](commands.md) · [Themes](themes.md)

```sh
octet --safe-mode                           # interactive
octet --plain --safe-mode                   # plain, in order
octet -p "Explain this function" --no-tools # final answer on stdout
octet --mode rpc                            # JSONL for automation
```

<a id="choose-a-frontend"></a>

## Choose a mode

| Mode | Use it for |
| --- | --- |
| `octet` | Streaming, tools, pickers, branching, steering and native scrollback. |
| `octet --plain` | Basic terminals, logs, accessibility tools, or no cursor control. |
| `octet -p "prompt"` (or `--print`) | The final answer on stdout, for scripts. |
| `octet --mode rpc` | JSONL commands and responses over stdin and stdout. |

On native Windows, the console (Windows Terminal or conhost), not `TERM`, picks
the frontend. See [Windows](windows.md#terminal).

Tern's `TERM=stencil-term` is recognized on Unix and VT-enabled Windows
consoles, so its own terminal identifier does not force the plain frontend.
`TERM_PROGRAM=tern` then selects native surface negotiation; `--tern off` keeps
the ordinary formatted ANSI UI. This detection support does not qualify every
Tern version's native protocol or Windows terminal rendering.

If a capable terminal unexpectedly shows plain output, check `TERM`, `NO_COLOR`,
`OCTET_COLOR` and the `plain` configuration/`--plain` flag. `TERM=dumb`, unknown
terminal identifiers and redirected input/output retain conservative fallback;
forcing `--color always` does not enable cursor control. `NO_COLOR` suppresses
styling without disabling the interactive layout. Preserve the terminal's real
identifier rather than globally overriding `TERM` to hide a detection problem.

The first three share one agent loop, providers, sessions, safety policy and
cancellation. Print mode still has tools, so add [tool limits](tools.md) if you
want none. Plain and print send readiness diagnostics to stderr. Plain outcomes
use explicit `[completed]`, `[failed]`, and `[interrupted]` labels with the shared
compact duration format: `1.2s` or `2s`, omitting fractional trailing zeroes.

[Cache warming](cache-warming.md) defaults to `streaming`; `idle` also runs while
the retained host waits for input, without delaying input, reload or shutdown.
Refresh usage is billed to the session, never the current assistant turn or its
cache-hit/throughput/context metrics. `show_cache_miss_notices = true` in user
config opts into brief ordinary cache-miss/refresh notices (default `false`).
`/session`, `/cache` and `/cache-warming` show scheduler state, expected savings,
miss penalty and refresh costs even with notices off. Plain/print notices stay
on stderr, and print stdout stays response-only.

The startup card says `permissions: full access` in bold red by default. With
`--safe-mode` it says `safe mode` (blue in the default theme) and octet asks
before running bash or changing files. Neither is a sandbox.

<details>
<summary>RPC mode</summary>

RPC isn't a terminal UI. Its messages use `type` fields, not the native host's
`hello` envelope, and `--mode rpc` can't be combined with `--print`. It's
independent of [native-host protocol 1](sdk.md) and [extension API
0.4](extensions/API-0.4-REFERENCE.md). The framing is Pi-compatible, but that
doesn't mean every Pi command or feature works.

`get_fork_messages` lists prompts from the session tree. `fork` creates a
new session before the selected prompt and returns its text; `clone` copies
through the current leaf. Both wait until the current turn settles and honor
session-change vetoes. `export_html` writes a redacted HTML export, refuses an
existing destination, and returns its path. `clear_queue` returns the original
text of removed steering and follow-up messages; it can't undo an input that
has already started committing to the session.

A finished tool's `tool_execution_end` result and its `toolResult` message carry
Pi's `details`: a Pi-compatible tool's own details, otherwise an extension tool's
non-model-visible metadata. Structured content and media parts are included when
the tool returned them. `get_state` reports `messageCount` without projecting the
history, so it stays fast on long resumed sessions.

</details>

Startup keeps routine session lookup, replay and extension-loading progress off
screen. You can type while startup work finishes, and the welcome card and saved
conversation appear when octet is ready. Pressing Enter on a prompt or slash
command while startup or another temporary operation owns input holds it without
adding a notice to the transcript. It runs once when the session is ready, and a
slash command is never sent as model input. A second Enter keeps the draft;
Esc or Ctrl+C restores it, as does a change of session or model before dispatch.
Setup prompts, errors, and cancel or shutdown diagnostics stay visible. Fresh
sessions skip the replay worker entirely, and resumed sessions still restore
their history. `--models` inventory
discovery also runs after the shell owns input, not before the first paint.
Fast ANSI startup coalesces into the resolved first frame, including screen
entry, title and cursor. The existing 100 ms idle-poll interval bounds how long
an unresolved initial paint can be coalesced; readiness, typing, setup panels,
errors and cancellation bypass that budget. It is not a minimum loading time,
and input never waits for cosmetic work. Slower initialization keeps the neutral
composer editable without provisional model branding or changing its geometry.
With `OCTET_STARTUP_TRACE=1`, stderr's `frame.ready` marks resolved native
session/input ownership independently of splash and footer labels; it does not
mean extension startup has settled. `frame.first-written` and
`frame.editable-written` mark completed first ANSI output; `frame.ready-written`
marks resolved output. `submission.ready` marks the first native submission
boundary after extension startup settles. `frame.settled-written` marks painted
native chrome with the extension-loading status cleared. `title.written` follows
the transaction containing the first OSC 2 title. These are application/write
boundaries, not emulator pixels or title-bar display timestamps. Redirect stderr
to a file for off-screen traces. See the [handoff audit](benchmarks/startup-handoff.md). Ctrl+D restores the starting terminal
settings, including under tmux after extension reload and resize.

An extension's custom editor, footer, header, or widgets that appear during
startup (for example a Pi extension's `setEditorComponent` in `session_start`)
take over only after the theme its extensions select (such as your Pi theme) is
installed. Until then the native composer stays in place and editable, and what
you typed carries over into the custom editor. Setup dialogs still appear
immediately. See [remote UI](extensions/remote-ui.md#palette-handoff-for-persistent-chrome).

Fresh-start composer rows are reserved before the welcome, without provisional
model branding. The default welcome uses a small 8×2 byte mark; narrow panes
stack its details rather than squeezing permissions and exit help beside it.
It stays static and fits the pane height. Typing and late update hints do not
move it or clear saved history. Cursor placement is part of
the synchronized frame; Working and Thinking animations remain active. Enter
before model/session resolution keeps the draft without submitting or queuing
it. Submit after resolution.

<details>
<summary>Window title and background detection</summary>

After session resolution, the interactive renderer sets the terminal window
title to `octet` or `octet · <session name>` (via OSC 2). It updates on rename
and session changes, and nothing is written to plain, print or RPC output. The
first title shares the synchronized transaction with the resolved frame; a
terminal or multiplexer can still present its heading later. When auto-theme
detection needs an OSC 11 background-color reply, the probe starts alongside
initialization, without waiting before the first paint or submission. Late
replies use the ordinary input owner to resolve Auto contrast without changing
the draft or layout. Missing or unsupported replies keep the initial theme;
explicit contrast and file-theme selections remain authoritative.

</details>

## Tab identity and activity

The interactive frontend reports `app=octet` and its actual host-owned activity
using [OSC 7501 v0.4](https://www.superlogical.com/rex/docs/build/program-status):
`idle`, `working`, `blocked` (`permission`, `question`, or `auth`), `done`, and
`error`. Provider waits/retries remain working, not blocked. Tool/extension input
is a question, including private input; a secret field alone does not prove a
login. ChatGPT browser/device authorization reports auth while awaiting the user,
then working while exchanging/saving and reloading the model catalog. The login
operation reports done only once Codex models are ready, or error if reload fails.
Only authoritative run or foreground operation settlement reports done/error;
cancellation reports idle. Ordinary reports and pickers do not block an otherwise
running agent.

Reports are deduplicated across frames and renderer replacement, not across
separate foreground operations that finish between paints. Suspension and
exit retire live working/blocked activity to idle, but preserve done/error for
the terminal to acknowledge. Resumption reasserts live activity. Changing between
primary and alternate screens does not clear the terminal's record. Progress is
indeterminate: no percentage, completion metric, prompt, tool argument, private
approval detail, error text, login URL or OAuth code is sent. OSC 2 still carries
the existing session/window title, independently of activity.

No detection query, input reader or startup wait is added. Unsupported terminals
ignore the OSC. Plain, print, RPC and redirected output remain free of these
controls. A multiplexer must forward/support the protocol; octet does not change
shell or multiplexer configuration.

| Terminal | Native identity/status | octet logo in tab |
| --- | --- | --- |
| OSC 7501 terminal | Stable `app=octet`; full activity states/kinds | The protocol identifies an app; it does **not** carry an image. |
| Superlogical Rex | Real local Rex Beta build 1541 server recorded `app=octet` with idle, working, blocked/permission, done and error. Quitting live working/blocked activity reports idle; done/error survive child exit. | The installed block-glyph catalog has no `octet` asset, and no supported runtime icon registration route was found. A branded mapping belongs in Rex and should use the [canonical mark](assets/octet/README.md). `app=` identifies the app; it is not image data. |
| Tern | Existing TSP `hello.app=octet` identifies a listening surface's pane; OSC 2 keeps the session title. Tern's documented OSC 9;4 channel additionally receives indeterminate working, paused user gates, error, or inactive. OSC 7501 is also emitted; Tern support for that newer protocol is not assumed. | Its built-in app/icon catalog must add/map octet's mark. A welcome image is **not** a tab icon. See [Tern](tern.md#tab-and-pane-identity). |

Rex and Tern choose their own tab presentation. App-side wire/PTY tests do not
qualify a specific terminal version's pixels, notifications, or interaction.
Rex Beta build 1541's server-side records were checked using task-owned blocks
and a frozen diagnostic octet candidate, not a released build. Its rendered tab
glyph, unfocused-tab indicators and OS notification delivery were not observed.
No terminal plugin, global theme, proprietary escape sequence, or replacement
logo is installed.

## Input and active work

Type `/` for [commands](commands.md) and `@` for file mentions (`.gitignore` is
respected). A `./`, `../`, `~/` or absolute path token starts filesystem
completion. Up and Down pick a visible path or mention suggestion, and Tab
inserts the selected result. Directory completion stays open, and spaces are
backslash-escaped. Without a visible completion menu, the arrows move through
the editor as usual. Multiline editing, bracketed paste and large-paste chips
work. A pasted or dropped file needs an attachment chip before you send it: [a
typed path is just text](media.md#attach-explicitly). Undo and redo each keep at
most 64 snapshots / 4 MiB, oldest first. An edit too big for that budget starts
a new undo boundary instead of keeping an unbounded copy.

With mouse capture on, a left click in the native composer places the cursor at
that visible character, including wrapped or scrolled multiline drafts and while
an answer streams. Composer and completion hit boxes include the working slot
above them and keep the pressed geometry through streaming repaints. Wide
characters and combining marks stay intact. Attachment
and large-paste chips snap to their nearest boundary. Clicking beyond a wrapped
segment stops at its last character; beyond a logical line it goes to line end.
Dragging over composer text selects it without moving the edit cursor; release
copies the displayed text, not a chip's hidden payload. Wheel or trackpad scrolling
keeps an active composer drag and its copy-on-release gesture, while cancelling
pending clicks that have not become a drag. Completion-list clicks
accept the clicked suggestion instead of moving the cursor. Borders and blank
composer rows leave the cursor unchanged. Native Tern uses its own editor caret
events with the same atomic-chip rule. Pi custom editor components own their
cursor; captured cell clicks do not reposition octet's hidden native mirror, and
component mouse positioning is not currently bridged.

While octet works:

- Enter queues live steering for the next model boundary.
- Ctrl+S queues a local follow-up. Follow-ups run one at a time, in order, after
  normal completion. Both kinds of pending input share the bounded hint above
  the input.
- Option+Up (Alt+Up) recalls the newest editable queued steering message or
  follow-up into an empty input, keeping attachment and paste chips. A recalled
  steering message is withdrawn from the agent before it's saved, so it's never
  also delivered. Once the agent has claimed it at a model boundary, the recall
  is refused and it stays queued. Recall never submits, never interrupts and
  never overwrites a draft.
- Escape first closes the current panel or slash popup. With the input focused,
  it interrupts active work and sends the oldest queued follow-up **after** the
  run settles. It never submits an unqueued draft.
- Ctrl+C clears a draft. With no draft, it aborts active work without sending
  anything; once the run has stopped, another Ctrl+C closes octet, as it does
  at any idle empty prompt. A Ctrl+C abort also cancels a send that
  Escape had armed. Failures and closing leave follow-ups unsent.
- Ctrl+D closes octet from any input, after active work and child processes are
  cleaned up.
- Shift+Enter adds a newline when the terminal reports it distinctly. Ctrl+J
  also inserts a newline.

### Shift+Enter terminal support

On Unix, octet requests Kitty keyboard disambiguation on both the primary and
alternate screens. Shift+Enter must arrive as CSI-u/Kitty `\x1b[13;2u` (optionally
with a Kitty event type) or xterm `modifyOtherKeys` `\x1b[27;2;13~`. Octet accepts
both forms, but does not request `modifyOtherKeys` mode: the terminal or
multiplexer must be configured to send it. See [tmux setup](tmux.md).

Some terminals send the same carriage return (`\r`) for Enter and Shift+Enter.
Octet cannot distinguish those keys: Shift+Enter will act as Enter and may submit
rather than insert a newline. Use Ctrl+J instead, or configure a distinguishable
encoding. Native Windows uses console key records rather than these byte
encodings; Shift+Enter depends on the console host ([Windows](windows.md#terminal)).

Pi custom editors use the same native submission rules, including slash and
extension commands. Their resolved Enter/follow-up keybindings are honored, and
a rejected submission retains the genuine Pi Editor's draft, pastes and undo.
Terminal bracketed paste into a Pi custom editor normalizes CR and CRLF line
endings to newlines, without submitting the draft. The original byte bound and
rejection of other terminal controls remain enforced.
The custom editor occupies the composer slot, not the screen: Octet keeps the
transcript, chrome, queue indicators and one native slash popup. Native, executable
extension and Pi commands share that popup; later name collisions are owner-qualified
(`/extension:name`). Ctrl+G opens the external editor for an idle native draft.
With a Pi custom editor mounted, the key goes to terminal input listeners and
then to that editor instead. Ctrl+G still returns from explicit fullscreen views.

Native clipboard reads during active work don't block input, cancellation or run
progress. A pending read is dropped when the run settles or is cancelled, or
when the draft is cleared, sent, steered, recalled or consumed by a command.
Results are also dropped after text or cursor edits in between, or if the input
loses focus, so a late clipboard result can't land in a replacement draft or
another input. See [all keys](commands.md#keys).

## Scrolling and rendering

```sh
octet --color auto
octet --mouse app   # always capture the mouse, including inline
```

By default octet uses the fullscreen (alternate-screen) surface with `mouse = "auto"`.
Fullscreen has no terminal scrollback, so `auto` captures the wheel/trackpad and
drag selection there: scrolling and selecting work with no flag, as in Pi.
`--tui-mode inline` (or `tui_mode = "inline"`) keeps octet on the primary screen,
where `auto` leaves mouse reporting off so your terminal keeps drag selection and
wheel/touchpad history. `--mouse terminal` and `--mouse off` never capture the
mouse on either surface; `--mouse app` always does. Inline, the renderer follows
the height of the content, not a fixed full-screen composer and footer; it loads
the complete resumed branch and appends to native scrollback. Explicit
`--mouse app` (or `mouse = "app"` in configuration) opts inline sessions into the
bounded semantic viewport as well, and lets a resumed session load newest-first.
Explicit CLI choices take precedence over configured mouse policy. Switching
surfaces with `/settings tui fullscreen` or `/settings tui inline` immediately
reapplies that policy: `auto` starts or stops capture with the surface, while
`app`, `terminal`, and `off` keep their explicit ownership.

A `pi` theme changes presentation, not viewport ownership. Native Tern surfaces
negotiate their own inline layout and leave ANSI mouse reporting off, including
with `--mouse app`, so Tern owns touchpad scrolling and selection without Shift.
If native rendering fails, the ANSI fallback restores the requested mouse policy.
`--tern off` uses the ordinary ANSI policy even inside Tern. Native pickers and
report overlays do not change the ordinary ANSI launch default.

With mouse capture on, click the model, thinking level, or context meter in the
built-in footer to open the same surface as `/model`, `/thinking`, or `/context`.
Click the same label again to close it without changing anything. Your draft stays
in the composer. Only labels that fit and are visible are clickable.

With capture on (the fullscreen default, or `--mouse app`), wheel/touchpad events scroll the transcript by
one row per terminal line event (Alt/Option: fifteen); bursts accumulate without
changing composer history. Each event keeps that fixed step: the terminal converts
high-resolution trackpad motion to line reports and applies its own acceleration;
octet preserves every report without a second multiplier, curve, smoothing, or
inertia. Each event immediately notifies the renderer; only pending paints
coalesce, never the line deltas. Pixel mouse mode (1016) changes pointer
coordinates, not wheel-delta precision, and stays disabled. Dragging the
scrollbar keeps the pointer until you release it; wheel events still move the
transcript over the track, scroll hint, and viewport edges during that press.
Scrollbar hover, track clicks and dragging use the painted track and stay
responsive during idle updates and streaming, without waiting for pending output.
The thumb stays thin on hover and drag. Pressing it leaves the reading position
unchanged until you drag; clicking the track jumps. In `auto`, the bar overlays
card backgrounds without changing transcript wrapping. `always` reserves a gutter.
Holding a selection drag at the top or bottom edge auto-scrolls, including
between pointer-motion events. Wheel/trackpad scrolling during a drag extends
the range at the pointer; scrolling after release extends the retained range
and refreshes its clipboard copy. Click-drag highlights transcript text without
Shift. Release immediately copies it and briefly shows **Copied selection**;
Ctrl+Shift+C copies the current selection again. Selecting in the transcript,
composer or splash clears the previous surface's range and pending gesture;
Ctrl+Shift+A selects only the transcript. Double-clicking splash text selects
one word on its row and copies it once on release. Byte-mark clicks clear the
selection without copying. Streaming text, tool completion,
working animations and worker updates preserve semantic endpoints outside their
changes. Focus changes, resize and replacement surfaces cancel an active drag,
so a late release cannot copy a previous range. Reflow under
a stationary pointer does not expand the selected range on release; a displaced
release still counts as movement when terminals coalesce drag events. Ctrl+C keeps
its draft-clear/interrupt/exit meaning. Composer, footer and separator rows are not
transcript text. Shell selections include the retained, progress-normalized,
sanitized command output, rather than mapping output rows to the command header.
Each wrapped output row maps only its visible fragment, including wide Unicode
text; selecting a continuation does not jump back to the first row. Compact-tail
selection starts at the retained source boundary, not at matching text in omitted
output. Elision labels and empty-output placeholders are not copyable source.
Bash and Codemode tool selections and edit/write diffs copy the sanitized text
presented at the current disclosure level, including content scrolled out of
view. Ctrl+O reveals omitted input/output before it can be copied; collapsed
group children, private tool arguments, capture metadata, line-number gutters
and disclosure hints stay out of the copy. Repeated or decorated Unicode rows
select their own occurrence, not matching text elsewhere in the transcript.
Shift+drag remains your terminal's native selection override
where the terminal supports it.

### Markdown selection copy

Rendered Markdown hides HTML comments, but keeps comment syntax inside code.
Assistant selections copy the corresponding retained original Markdown source,
never Markdown regenerated from plain text. Fully selected headings, emphasis,
links, list items, fenced code blocks (including language), and table rows keep
their original syntax. Selected list markers keep their original spelling even
for a partially selected wrapped item. Partial selections copy the source
characters behind the selected rendered characters: inline emphasis, code-span and link delimiters are
included only when their entire rendered content is selected. Partial code-block
selections copy raw code without fences. Partial table selections copy selected
cell source text, separated by tabs within a row and newlines between rows;
only whole rows retain their original `|` separators. Narrow table labels are
presentation, not cell source. Repeated text maps to the selected occurrence.
Math uses the renderer's transformation: a whole equation keeps its original
source, while a partial equation copies its selected displayed characters.
Displayed link destinations and other spans without source provenance copy the
selected painted text, including when mixed with mapped Markdown spans.
Streaming and reflow preserve source endpoints, or explicitly clear the selection
when that mapping changes. Other blocks keep their existing text copy, and mixed
selections keep transcript order.
Source provenance is prepared with layout, not reparsed or cloned in full on
pointer movement.

Literal whitespace-delimited HTTP(S) URLs in shell output retain their complete
targets across wrapping, without interpreting output as Markdown. Encoded targets
over 2 KiB stay plain; decoration also has a 64 KiB wrapped OSC-payload budget per
shell output render, after which additional URLs stay plain.
With capture on, a plain left-click on an HTTP(S) transcript link opens it in
an available system browser on release, without Shift or Ctrl. Dragging a link
selects/copies instead; moving away and back does not activate it. Resize,
scrolling, focus changes and intervening input cancel the pending click. Octet
never launches local-file or application-protocol links this way. With capture
off, including native Tern, link gestures remain terminal-owned. Captured clicks
also work without terminal OSC 8 support; unsupported hyperlink controls are
omitted from terminal output, not from Octet's retained hit targets.

Copy writes to an available local clipboard helper and emits OSC 52 to the
terminal, so SSH/remote sessions can reach the client clipboard. The terminal must
permit OSC 52; inside tmux enable `set -g set-clipboard on`. Remote transport sends
a UTF-8-safe prefix of at most 49,152 source bytes (64 KiB base64 payload), using
BEL termination. A brief **terminal copy truncated** message reports when that
limit applies. The retained selection and local clipboard copy are not truncated
by that transport limit. Capture explicitly requests cell coordinates (SGR 1006),
clearing inherited pixel reporting (1016) and alternate-scroll arrow emulation
(1007); octet does not infer cells from pixel reports.

In the semantic viewport, even one upward wheel step disengages follow-tail and
pins the reader's position through repaints, streaming and working-row or worker
roster height changes. There is no near-bottom snap zone. Scrolling back to the
exact bottom resumes following new output. The centered `[ ↓ Jump ]` button
above the input also returns to live output. It shows a count when space permits,
for example `[ ↓ Jump · 3 new ]`. Click any painted button cell, not the blank
space around it, while idle or streaming, including with worker activity visible.
Pending output never disables the painted button or changes its hover target.
Hover underlines the control when motion is reported; no-color/ASCII profiles
retain a bracketed text button. PageDown and Ctrl+End keep their keyboard paths.
Wheel history that octet doesn't capture stays with your terminal,
because portable protocols can't report its position.

The renderer uses a complete retained frame, synchronized frames, and exact
first-to-last changed-range repainting. Completions, panels, reports, and streamed
Markdown participate in the same algorithm; visible changes do not force history
replay. Dimension changes and canonical changes above the old viewport clear and
replay the complete native transcript, so saved and live output remain complete.
This can reset your terminal's reading position, and replay work grows with
history. PageUp and explicit `--mouse app` instead use a separate bounded semantic viewport.
Resize bursts settle after 75 ms of quiet (150 ms maximum); width changes reflow
the transcript, while height-only changes reuse its wrapping. Away-and-back
resizes also rebuild native history. The hardware composer cursor stays visible
inside synchronized frames through panels, resizing, and renderer resumes.
The terminal controls the cursor's shape and blink; octet doesn't override
those settings on startup or reset them on exit.
Fullscreen paints address only visible terminal rows, without scrolling newlines
or row insertion. Offline virtual-terminal checks cover every paint through
working-message rotation (including wide glyphs and full-width rows), spinner
ticks, reasoning and tool start/finish, and footer status changes. These checks
verify emitted rows, not Ghostty's physical frame presentation.
[Rendering details](design/octet-tui.md#terminal-guarantees).

For a physical Ghostty scroll check, use a long resumed session with images and
tool cards: make tiny up/down gestures at the live tail and reverse direction;
repeat over the hint, top/bottom edges and scrollbar, including while holding a
scrollbar press. Try a continuous sweep and stop, Alt/Option scrolling, edge-drag
selection and copy, then PageDown back to live. While output arrives, an older
reading position should stay pinned and the live tail should follow. Repeat at
normal and short pane heights with your usual extensions. PTY timings do not
qualify those physical gestures or image placement.

Layouts adapt between wide and narrow terminals and fall back from truecolor to
256 colors, 16 colors and no color, and from Unicode to ASCII. Markdown shows
highlighted code, tables, task lists and links, plus bounded summaries of tool
intent and lifecycle. Control sequences in untrusted text are sanitized. The
vendored `sexy-tui-rs` renderer uses `#![forbid(unsafe_code)]`.
[Themes](themes.md).

## Reasoning and progress

`/thinking` can change an explicitly qualified Responses route without stopping
the root task. A queued label lasts until the durable next-response boundary.
The effective choice is separate from the pinned wire baseline, and neither is a
provider acknowledgement. Other routes still change at an idle boundary. See the
[control
qualification](provider-thinking.md#mid-conversation-changes-unreleased).

Reasoning is collapsed by default. Ctrl+T shows or hides what octet kept,
for every trace in the transcript and for new ones. It is separate from
Ctrl+O, which owns tool and shell output. In native Tern, each key also resets
only its own mode's section clicks, preserving the other mode's disclosure.

In the default ANSI layout, one live item appears at the end of the transcript.
In fullscreen, it scrolls out of view with the transcript; Jump returns to the
live end. Queued steering and follow-ups stay above the composer.
`Working` and `Thinking` share the run timer and occupy the same two lines.
One blank line separates the latest event from the indicator. For a shaded
prompt, that blank line is outside the card, not part of its colored edge.
One blank line separates the summary line from the composer. The summary line stays empty while
Working, so a Thinking update doesn't move the composer. On short terminals,
padding yields to history and controls. These lines do not enter saved history,
search, or copy. While the model reasons, the summary shows its latest Markdown heading with a dim
expand hint, or the hint alone if there is no heading. Expanding the live trace
hides both indicator lines; collapsing it restores them. Ordinary reasoning text
never becomes a label. With **Tool call and thinking durations** on in Settings,
a collapsed, finished trace shows a muted `• Thought for 15s` marker instead of the
live heading. Traces under a second show rounded milliseconds, such as
`• Thought for 185ms`. With durations off, the marker takes no space. Ctrl+T
reveals the trace in either case. Thinking time is measured live, not recreated
when a session is resumed.

The [Pi presentation profile](themes.md#pi-presentation-profile) can put Working
in the composer rule and show reasoning in the transcript instead.

Provider lifecycle, compaction and retry labels temporarily own the same slot.
Retry status keeps the interrupt hint but omits the run timer, so two clocks
never compete. Pi working components can customize the Working indicator. Run
settlement clears the slot.

```text
• Thinking (7s · Esc to interrupt)
  └ Verifying the implementation (Ctrl+T to expand)
```

Reasoning and activity dots keep a solid, fixed-size glyph. The `Working` dot
shares the label's phase and blinks as the shimmer crosses it, then returns to
its resting colour. The live `Thinking` dot keeps its model colour; the settled
`Thought for` dot is muted. Tool and shell dots turn green on success or red on
failure, and assistant-response dots stay steady. To change the level, see
[Selecting reasoning](providers.md#reasoning).

<details>
<summary>Shimmer details</summary>

By default the shimmer is a smooth, asymmetric raised-cosine band with a central
glint, on known dark and light TrueColor and ANSI256 terminals. Activity shimmer
is foreground-only and parks briefly after crossing the whole label. Set
`OCTET_SHIMMER=classic` for the old stepped field. ANSI16, unknown-background,
reduced-motion and no-color paths keep the compatibility or static behavior.
Expanded reasoning keeps its indent, with no margin dot or extra first bullet.
Collapsed reasoning adds no stored row; only enabled timings can leave a marker.

</details>

<a id="tool-evidence-and-worker-activity"></a>

## Tool output and workers

Bash and PowerShell commands wrap to the available width. In terse mode, they
show up to twelve rendered command rows, then a count of hidden rows. Generic tool
input previews use the same twelve-row limit for their selected argument, preserving
newlines. Ctrl+O expands the full retained input and collapses it again. That
doesn't change tool execution or the separate output preview.

In fullscreen with mouse capture, click a tool's header text to expand just
that card. Click it again to return to its compact preview. Dragging still
selects text, and links keep their own click action. Native Tern tool headers
also toggle one card, subject to Tern's [command-output gate](tern.md#integration).
Ctrl+O changes disclosure across the transcript and
resets individual tool choices; Ctrl+T changes reasoning, not tool cards.
A nested Codemode status row belongs to its parent; it is not another header.

`--show-images` (or `show_images = true`) shows tool-result images inline. It's
off by default and works on Kitty-compatible terminals. Others fall back to
text. It never loads a URL or path, copied, plain and print output never contain
image data, and it doesn't upload anything or replace [attachment
consent](media.md#attach-explicitly). The model must still support the format.

Active shell cards use the event dot and show real output as it arrives. In the
default layout with durations enabled, `└ Took …` aligns with the tool's output
elbow below any inline image preview. Fullscreen image crops keep the original tool gutter when their first row scrolls
above the viewport. Only the visible image rows are placed, leaving the composer
and footer outside the placement. One fullscreen placement tracker handles native
image anchors and raw Kitty graphics rows: scrolling, cropping and redraws retire
old placements before placing images at their current rows. On Kitty-compatible
terminals, scrolling reuses the data of images that stay on screen by ID. An
image that leaves the viewport is freed and sent again when it returns, as are
visible images after a full redraw, a screen change or an unconfirmed write.
Status updates away from an image leave its placement alone. Pi-compatible composer/footer components
share this terminal painter; they do not write a separate graphics frame.
Terminal-specific pixels still require separate qualification; protocol and
retained-row tests are not Rex or Ghostty verification.

Ctrl+O reveals compaction, subagent activity, bounded search, tool and shell
output, and edit and write diffs; Ctrl+T reveals the reasoning traces.
`/verbose [on|off]` steers both. Reasoning display is a per-session choice
that survives new traces, and a theme's `thinking_expanded` decides the
starting state. They can't recover
output that was discarded when it was captured. Raw arguments, unsanitized
failures and extension-rendered payloads stay out of the transcript and out of
copy. A failed run keeps `failed · <duration>` and a short reason with
credentials removed.

Tool results the model needs are saved and sent to the provider. Live progress
is neither. Replaceable tool-progress annotations use one clipped row, so phase
labels of different lengths keep the transcript still; captured output retains
its normal wrapping and disclosure. When a Pi-compatible shell renderer finishes,
its last approved snapshot stays visible until the final renderer reply replaces
it in one paint, including when the assistant or another tool continues meanwhile.
Resize, theme, disclosure and renderer-owner changes still invalidate that snapshot;
a failed or unavailable renderer uses the native result card.
[Tool presentation](design/octet-tui.md#tool-presentation).

While workers are active, a bounded, tool-like **Subagents** block in the
transcript updates in place. It no longer stays pinned above the input. Its
heading counts worker states, and up to four child lines show active tasks with
input and output tokens. Only the wave's peak used detail rows stay reserved
until settlement, including when a sibling fails or finishes; retained workers
never add unused slots. Repeated waits and status checks keep unchanged history
compact, and consecutive roster bands share one spacing row. Older host
snapshots and cached inspector projections do not replace newer live telemetry.
Ctrl+O keeps disclosure. `/subagents` opens the live worker list, including
during a run, with the complete roster (up to 32) and
failure details. Enter opens a worker's read-only transcript and Ctrl+X stops
the selected worker. `/subagents stop <name-or-id|all>` also works during a run;
other worker operations wait for idle. `/extensions` manages extension
enablement and configuration instead.
Finished workers' usage is added once to the main session's ledger, so cost
limits count it. [Worker
display](../extensions/octet-subagents/README.md#tui-and-serve-presentation).

<details>
<summary>How worker activity is counted and shown</summary>

- List rows keep state, model and available metrics. The `tools` column counts
  tool calls, not model turns.
- Host `limit_reached` counts as failed. `interrupted`, `shutdown`, `detached`
  and `awaiting_approval` count as stopped. Detached and approval-parked workers
  stay recoverable, not successful, and their exact states and reasons stay
  inspectable after the block settles.
- Raw first-party orchestration calls and results, including errors, stay out of
  the interactive transcript, live and on replay, and worker state or reason
  transitions add no automatic notices. This doesn't change worker outcomes,
  model-visible errors, durable results, approval prompts, or ordinary tool and
  run failures.
- Resume starts a fresh telemetry roster rather than replaying raw calls.
  Already committed child cost isn't added again when telemetry refreshes.
- The block's input tokens sum uncached, cache-read and cache-write usage, and
  output estimates are marked until usage settles. Tool-call counts, priced
  spend, transient phases and tool identities stay in the inspector. The
  nonblocking 250 ms refresh keeps its last fenced snapshot on failure, and no
  extension replaces the cumulative footer.

</details>
