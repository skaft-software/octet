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

The first three share one agent loop, providers, sessions, safety policy and
cancellation. Print mode still has tools, so add [tool limits](tools.md) if you
want none. Plain and print send readiness diagnostics to stderr.

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
doesn't mean every Pi command or feature works, and it hasn't been tested
against a pinned Pi release.

</details>

Startup keeps routine session lookup, replay and extension-loading progress off
screen. You can type while startup work finishes, and the welcome card and saved
conversation appear when octet is ready. Setup prompts, errors, and cancel or
shutdown diagnostics stay visible. Fresh sessions skip the replay worker
entirely, and resumed sessions still restore their history. `--models` inventory
discovery also runs after the shell owns input, not before the first paint.

Fresh-start composer rows are reserved before the welcome, without provisional
model branding. The welcome is static and fits the pane height. Typing and late
update hints do not move it or clear saved history. Cursor placement is part of
the synchronized frame; Working and Thinking animations remain active. Enter
before model/session resolution keeps the draft without submitting or queuing
it. Submit after resolution.

<details>
<summary>Window title and background detection</summary>

After session resolution, the interactive renderer sets the terminal window
title to `octet` or `octet · <session name>` (via OSC 2). It updates on rename
and session changes, and nothing is written to plain, print or RPC output. When
auto-theme detection needs an OSC 11 background-color reply, the probe starts
after the first ready frame. A changed background triggers a repaint, and a
timeout or an unsupported reply keeps the initial theme.

</details>

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
  anything, and does nothing when idle. A Ctrl+C abort also cancels a send that
  Escape had armed. Failures and closing leave follow-ups unsent.
- Ctrl+D closes octet from any input, after active work and child processes are
  cleaned up.
- Shift+Enter adds a newline if your terminal reports enhanced keys.

Pi custom editors use the same native submission rules, including slash and
extension commands. Their resolved Enter/follow-up keybindings are honored, and
a rejected submission retains the genuine Pi Editor's draft, pastes and undo.
The custom editor occupies the composer slot, not the screen: Octet keeps the
transcript, chrome, queue indicators and one native slash popup. Native, executable
extension and Pi commands share that popup; later name collisions are owner-qualified
(`/extension:name`). Ctrl+G returns from explicit fullscreen views only, never
unmounting the editor or footer.

Native clipboard reads during active work don't block input, cancellation or run
progress. A pending read is dropped when the run settles or is cancelled, or
when the draft is cleared, sent, steered, recalled or consumed by a command.
Results are also dropped after text or cursor edits in between, or if the input
loses focus, so a late clipboard result can't land in a replacement draft or
another input. See [all keys](commands.md#keys).

## Scrolling and rendering

```sh
octet --color auto
octet --mouse app   # opt in to in-app scrolling and drag selection
```

By default octet stays inline (`mouse = "auto"`) on the primary screen and
leaves mouse reporting off, so your terminal keeps drag selection and
wheel/touchpad history. `--mouse terminal` and `--mouse off` also preserve native
gestures. Inline, the renderer follows the height of the content, not a fixed
full-screen composer and footer; it loads the complete resumed branch and appends
to native scrollback. Explicit `--mouse app` (or `mouse = "app"` in configuration)
opts into the bounded semantic viewport, captures the wheel and drag selection,
and lets a resumed session load newest-first. Explicit CLI choices take precedence
over configured mouse policy.

With capture on (`--mouse app`), wheel/touchpad events scroll the transcript by
three rows per event (Alt/Option: fifteen); bursts accumulate without changing
composer history. Click-drag highlights transcript text without Shift. Release
copies it, or use Ctrl+Shift+C to copy the current selection again; Ctrl+C keeps
its draft-clear/interrupt meaning. Composer, footer and separator rows are not
transcript text. Shift+drag remains your terminal's native selection override
where the terminal supports it.

Copy writes to an available local clipboard helper and emits OSC 52 to the
terminal, so SSH/remote sessions can reach the client clipboard. The terminal must
permit OSC 52; inside tmux enable `set -g set-clipboard on`. Remote transport sends
a UTF-8-safe prefix of at most 49,152 source bytes (64 KiB base64 payload), using
BEL termination. The retained selection and local clipboard copy are not truncated
by that transport limit. Capture explicitly requests cell coordinates (SGR 1006),
clearing inherited pixel reporting (1016) and alternate-scroll arrow emulation
(1007); octet does not infer cells from pixel reports.

Inline, PageUp pins the view above the tail while output grows, and shows when
there's new output. PageDown returns to live output. Wheel history that octet doesn't capture stays with your terminal,
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
[Rendering details](design/octet-tui.md#terminal-guarantees).

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

Reasoning is collapsed by default. Ctrl+O expands what octet kept.

Each run shows a shimmering `Working` row, and a trailing
`Working (<elapsed> • esc to interrupt)` stays until the run settles. Retry
status keeps the interrupt hint but leaves out the run-elapsed counter, even
after its backoff countdown reaches zero, so two clocks never compete on one
row. While the model reasons, the bold `Thinking` label shimmers more quietly
than `Working` on supported terminals. It shows the latest Markdown heading from
the reasoning (a `#` heading or a standalone bold line), with a dim expand hint.
Ordinary reasoning text never becomes a label. With no heading, only the hint
shows.

```text
• Thinking
  └ Verifying the implementation (ctrl+o to expand)
```

Reasoning and activity dots keep a solid, fixed-size glyph. The `Working` dot
shares the label's phase and blinks as the shimmer crosses it, then returns to
its resting colour. The `Thinking` dot keeps its model colour. Tool and shell
dots turn green on success or red on failure, and assistant-response dots stay
steady. To change the level, see [Selecting reasoning](providers.md#reasoning).

<details>
<summary>Shimmer details</summary>

By default the shimmer is a smooth, asymmetric raised-cosine band with a central
glint, on known dark and light TrueColor and ANSI256 terminals. Activity shimmer
is foreground-only and parks briefly after crossing the whole label. Set
`OCTET_SHIMMER=classic` for the old stepped field. ANSI16, unknown-background,
reduced-motion and no-color paths keep the compatibility or static behavior.
Expanded reasoning keeps its indent, with no margin dot or extra first bullet,
and completed reasoning disappears again once collapsed.

</details>

<a id="tool-evidence-and-worker-activity"></a>

## Tool output and workers

In terse mode, a Bash command shows its first three rendered lines, then a count
of hidden command lines. Ctrl+O expands the full retained command and collapses
it again. That doesn't change the executed command or the separate output
preview.

`--show-images` (or `show_images = true`) shows tool-result images inline. It's
off by default and works on Kitty-compatible terminals. Others fall back to
text. It never loads a URL or path, copied, plain and print output never contain
image data, and it doesn't upload anything or replace [attachment
consent](media.md#attach-explicitly). The model must still support the format.

Ctrl+O and `/verbose [on|off]` reveal reasoning, compaction, subagent activity,
bounded search and shell output, and edit and write diffs. They can't recover
output that was discarded when it was captured. Raw arguments, unsanitized
failures and extension-rendered payloads stay out of the transcript and out of
copy. A failed run keeps `failed · <duration>` and a short reason with
credentials removed.

Tool results the model needs are saved and sent to the provider. Live progress
is neither. [Tool presentation](design/octet-tui.md#tool-presentation).

While workers are active, a bounded, tool-like **Subagents** block in the
transcript updates in place. It no longer stays pinned above the input. Its
heading counts worker states, and up to four child lines show active tasks with
input and output tokens. Ctrl+O keeps disclosure. `/subagents` opens the live
worker list, including during a run, with the complete roster (up to 32) and
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
