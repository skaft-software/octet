# Terminal

[Documentation](README.md) · [Commands and keys](commands.md) · [Themes](themes.md)

```sh
octet --safe-mode                         # Interactive TUI
octet --plain --safe-mode                 # Chronological output
octet -p "Explain this function" --no-tools # Final response on stdout
octet --mode rpc                          # JSONL automation frontend
```

## Choose a frontend

| Mode | Use |
| --- | --- |
| `octet` | Streaming, tools, pickers, branching, steering, and native scrollback. |
| `octet --plain` | Basic terminals, logs, accessibility tooling, or no cursor control. |
| `octet -p "prompt"` / `octet --print "prompt"` | Response-only stdout for shell composition. |
| `octet --mode rpc` | Pi-compatible JSONL commands and responses over stdin/stdout for automation. |

The interactive, plain, and print frontends share the agent loop, providers,
sessions, safety policy, and cancellation. Print mode does not remove tool
authority by itself; use [tool controls](tools.md) when needed. Readiness
diagnostics go to stderr in plain/print mode.

RPC is a separate automation frontend, not a terminal UI. Its command and response
messages use `type` fields, not the native-host `hello` envelope. `--mode rpc`
conflicts with `--print`. It is independent of both [native-host protocol 1](sdk.md)
and [extension API 0.4](extensions/API-0.4-REFERENCE.md). Pi-compatible framing does
not establish blanket command/feature parity or live qualification against a
pinned Pi release.

The startup card labels `permissions: full access` in bold red by default.
`--safe-mode` changes it to bold-accent `safe mode` (blue in the default theme)
and enables approval gates for bash calls and workspace mutation. Neither is
OS containment.

Startup keeps routine session lookup, replay, and extension-loading progress off
screen. The composer accepts typing while startup work finishes; the resolved
welcome card and saved conversation appear at readiness. Setup prompts, errors,
and cancellation/shutdown diagnostics remain visible. Fresh sessions skip the
replay worker entirely; resumed sessions still restore their history. `--models`
inventory discovery also runs after the shell owns input, not before first paint.
After session resolution, the interactive renderer sets the terminal window title
to `octet` or `octet · <user-assigned session name>` via OSC 2. It updates on
rename and session changes without placing controls in plain, print, or RPC output.

## Input and active work

Type `/` for [command discovery](commands.md), `@` for gitignore-aware file
mentions, or a `./`, `../`, `~/`, or absolute path token for filesystem
completion. Up/Down selects a visible path or mention suggestion; Tab inserts
the selected result. Directory completion stays open; spaces are
backslash-escaped. Without a visible completion menu, arrows retain normal
editor navigation. Multiline editing, bracketed paste, and large-paste chips are
supported. Explicitly pasted/dropped
media needs an attachment chip before submission; [typed paths alone are text](media.md#attach-explicitly).
Undo and redo each retain at most 64 snapshots / 4 MiB, evicting oldest history
first. An edit too large for that budget starts a new undo boundary rather than
retaining an unbounded copy.

Enter submits, or queues a local follow-up while work is active. Follow-ups
dispatch one at a time in FIFO order after normal completion. Ctrl+S instead
queues live steering for the next model boundary; both kinds of pending input
share the bounded hint above the composer. Option+Up (Alt+Up) recalls the newest
editable queued steering message or follow-up into an empty composer, preserving
attachment/paste chips. A recalled steering message is withdrawn from the Agent
before it is persisted, so it is never also delivered; once the Agent has claimed
it at a model boundary the recall is refused and the entry stays queued. It never
submits, never interrupts, and never overwrites a nonempty draft.

Native clipboard reads during active work do not block input, cancellation, or
run progress. A pending read is discarded when the run settles or is cancelled,
or when the draft is cleared, submitted, steered, recalled, or consumed by a
command. Results are also discarded after intervening text/cursor edits or loss
of composer focus, so late clipboard output cannot enter a replacement draft or
another input surface.

Escape first closes the current panel/slash popup; with the composer focused,
it interrupts active work and dispatches the oldest queued follow-up **after**
the run settles. It never submits an unqueued draft. Ctrl+C clears a nonempty
draft, otherwise aborts active work without dispatch and does nothing while idle.
A Ctrl+C abort also revokes dispatch previously armed by Escape.
Failures and close also leave follow-ups unsubmitted. Ctrl+D coordinates close
from any input surface, settling active work and child-process cleanup first.
Shift+Enter inserts a newline when the terminal reports enhanced keys.
[Full key table](commands.md#keys).

## Scrolling and rendering

```sh
octet --color auto
octet --mouse app
```

Default `mouse = "auto"`, explicit `terminal`, and `off` leave mouse reporting
disabled, preserving native drag selection and wheel history. The primary-screen
renderer follows logical content height, not a fixed full-screen composer/footer.
It materializes the complete resumed branch and appends into native scrollback.

PageUp claims a bounded semantic viewport in every mode. It stays anchored above
the tail while output grows and reports new output; PageDown returns to live
output. `--mouse app` selects that viewport from startup, additionally captures
wheel/drag selection, and permits tail-first lazy resume hydration. Uncaptured
wheel history stays terminal-owned: portable protocols cannot report its offset.

The renderer uses a complete retained frame, synchronized frames, and exact
first-to-last changed-range repainting. Completions, panels, reports, and streamed
Markdown participate in the same algorithm. Resize reflows the retained semantic
transcript, clears saved lines, and replays once; changes above the old viewport
also require full replay rather than leaving unwritten history. The hardware
composer cursor stays visible through panels, resizing, and renderer resumes.
[Rendering details](design/octet-tui.md#terminal-guarantees).

Wide/narrow layouts retain semantic structure with Unicode/ASCII, truecolor,
256-color, 16-color, and no-color fallbacks. Rich Markdown includes highlighted
code, tables, task lists, links, and bounded tool intent/lifecycle projections.
Untrusted terminal controls are sanitized. The vendored `sexy-tui-rs` renderer
uses `#![forbid(unsafe_code)]`. [Compiled model-aware theme](themes.md).

## Reasoning and progress

`/thinking` can change an explicitly qualified Responses route without stopping
the root task. A queued label lasts until the durable next-response boundary;
the effective choice is distinct from the pinned wire baseline. Neither is a
provider acknowledgement. Other routes retain idle-boundary selection.
[Control qualification](provider-thinking.md#mid-conversation-changes-unreleased).

Reasoning is collapsed by default; Ctrl+O expands retained content. Each accepted
run begins a bold, model-adaptive shimmering `Working` row. One trailing
`Working (<elapsed> • esc to interrupt)` remains even after assistant text until
the run settles; tool admission replaces it with the tool lifecycle. Retry status
keeps the interrupt hint but omits the run-elapsed counter, including after its
backoff countdown reaches zero, so two clocks do not compete on the same row.

While reasoning is active, the bold `Thinking` label shimmers more quietly than
`Working` on supported terminals. It shows the latest explicit ATX or
standalone-bold Markdown heading with a subdued expansion hint. Ordinary
reasoning body text is never promoted to a label; without a heading, only the
hint appears.

```text
• Thinking
  └ Verifying the implementation (ctrl+o to expand)
```

Expanded reasoning retains its inset without an event-margin dot or synthetic
first-line bullet. Completed reasoning disappears again when collapsed.
Reasoning/activity dots keep a solid, fixed-size glyph while their foreground
pulses with the label sweep: the default physical field is a smooth, asymmetric
raised-cosine band with a central glint on known Dark/Light TrueColor and
ANSI256 terminals. Activity shimmer is foreground-only and parks briefly after
crossing the complete label. Set `OCTET_SHIMMER=classic` for the legacy stepped
field; ANSI16, unknown-background, reduced-motion, and no-color paths retain the
compatibility/static behavior. Assistant-response dots remain steady; active
tool/shell dots pulse foreground/muted tones without changing size. Completed
success is green and failed tools red.
[Selecting reasoning](providers.md#reasoning).

## Tool evidence and worker activity

In terse mode, a Bash tool command shows its first three rendered lines, followed
by a count of hidden command lines. Ctrl+O expands the complete retained command
and collapses it again; this does not alter the executed command or the separate
output preview.

Worker activity appears in a bounded, tool-like **Subagents** transcript block
while workers are active. The block updates in place rather than staying pinned
above the composer; its heading counts worker states, and up to four child lines
show active tasks with input/output tokens. Ctrl+O retains disclosure, and
`/subagents` exposes the complete roster (up to 32) and failure details on
demand. Its list rows retain state, model, and available metrics; the `tools`
column counts tool calls, not model turns.
Host `limit_reached` belongs to the failed group; `interrupted`, `shutdown`,
`detached`, and `awaiting_approval` belong to the stopped display group.
Detached/approval-parked workers remain recoverable, not successful; their exact
states and reasons remain inspectable after the block settles. Raw first-party
orchestration calls and results, including errors, stay out of the interactive
transcript during live execution and replay. Worker state/reason transitions do
not append automatic notices. This presentation policy does not change worker
outcomes, model-visible errors, durable results, approval prompts, or ordinary
tool/run failures. Resume starts a fresh telemetry roster rather than replaying
raw calls. Already committed child cost is not added again on telemetry refresh.

`octet --show-images` (or `show_images = true` in user configuration) opts in to
bounded inline **tool-result display**, off by default. Validated inline image
payloads display on Kitty-compatible interactive terminals; unsupported terminals
fall back to text. Display never loads a URL or path, and copy/plain/print output
and terminal-write logs remain image-payload-free. This switch neither grants
automatic media upload nor replaces [explicit input-attachment consent](media.md#attach-explicitly);
model/codec support remains a separate requirement.

Ctrl+O and `/verbose [on|off]` disclose retained reasoning, compaction, delegated
activity, bounded search/shell output, and edit/write diffs. They cannot recover
bytes discarded by capture. Raw arguments/envelopes, unsanitized failures, and
extension-rendered payloads stay internal and out of transcript copy. Failed
runs retain `failed · <duration>` and a bounded terminal-safe reason; provider
diagnostics are credential-redacted before reaching the frontend.

Final structured tool results are persisted/provider-visible when needed to
continue the tool protocol. Live progress is neither persisted nor sent to the
model. [Tool presentation contract](design/octet-tui.md#tool-presentation).

Generic extension state is on demand. The subagents exception updates an
owner-scoped bounded transcript block while workers are active: state counts,
active tasks, and input/output tokens. Input sums uncached, cache-read, and
cache-write usage; output estimates are marked until usage settles. Tool-call
counts, priced spend, transient phases, and tool identities remain in the
inspector. The nonblocking 250 ms refresh retains its last fenced snapshot on
failure.
`/subagents` opens an arrow-key list; Enter opens a scrollable read-only child
transcript. No extension replaces the cumulative footer. Completed child usage
is mirrored once into the root ledger before settlement, including later cost
limits. [Worker presentation and accounting](../extensions/octet-subagents/README.md#tui-and-serve-presentation).
