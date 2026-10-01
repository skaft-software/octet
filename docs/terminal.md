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

- Enter queues a local follow-up. Follow-ups run one at a time, in order, after
  normal completion.
- Ctrl+S queues live steering for the next model boundary. Both kinds of pending
  input share the bounded hint above the input.
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

Native clipboard reads during active work don't block input, cancellation or run
progress. A pending read is dropped when the run settles or is cancelled, or
when the draft is cleared, sent, steered, recalled or consumed by a command.
Results are also dropped after text or cursor edits in between, or if the input
loses focus, so a late clipboard result can't land in a replacement draft or
another input. See [all keys](commands.md#keys).

## Scrolling and rendering

```sh
octet --color auto
octet --mouse app
```

By default octet leaves mouse reporting off (`mouse = "auto"`; `terminal` and
`off` do the same), so your terminal keeps drag selection and wheel history. The
renderer follows the height of the content, not a fixed full-screen composer and
footer. It loads the complete resumed branch and appends to native scrollback.

PageUp pins the view above the tail while output grows, and shows when there's
new output. PageDown returns to live output. `--mouse app` starts pinned,
captures the wheel and drag selection, and lets a resumed session load
newest-first. Wheel history that octet doesn't capture stays with your terminal,
because portable protocols can't report its position.

The renderer uses a complete retained frame, synchronized frames, and exact
first-to-last changed-range repainting. Completions, panels, reports, and streamed
Markdown participate in the same algorithm. Resize bursts settle before repairing
only live rows; width changes reflow the semantic transcript, while height-only
changes reuse its wrapping. Resize, PageUp, and historical repairs do not clear
saved lines or replay the complete history. Already-emitted native history stays
as snapshots; PageUp, copy, and the session contain the authoritative transcript
when a historical result changes. The hardware composer cursor stays visible
through panels, resizing, and renderer resumes.
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

Reasoning and activity dots keep a solid, fixed-size glyph while their
foreground pulses with the label sweep. Tool and shell dots turn green on
success or red on failure, and assistant-response dots stay steady. To change
the level, see [Selecting reasoning](providers.md#reasoning).

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
input and output tokens. Ctrl+O keeps disclosure. The worker list
(`/extensions`, then octet-subagents, then **Workers**, or just `/extensions`
during a run) shows the complete roster (up to 32) and failure details. Enter
opens a worker's read-only transcript and Ctrl+X stops the selected worker.
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
