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
and [extension API 0.3](extensions/API-0.3-REFERENCE.md). Pi-compatible framing does
not establish blanket command/feature parity or live qualification against a
pinned Pi release.

The startup card labels `permissions: full access` in bold red by default.
`--safe-mode` changes it to bold-accent `safe mode` (blue in the default theme)
and enables approval gates for bash calls and workspace mutation. Neither is
OS containment.

## Input and active work

Type `/` for [command discovery](commands.md), `@` for gitignore-aware file
mentions, or a `./`, `../`, `~/`, or absolute path token for filesystem
completion. Up/Down selects a visible path or mention suggestion; Tab inserts
the selected result. Directory completion stays open; spaces are
backslash-escaped. Without a visible completion menu, arrows retain normal
editor navigation. Multiline editing, bracketed paste, and large-paste chips are
supported. Explicitly pasted/dropped
media needs an attachment chip before submission; [typed paths alone are text](media.md#attach-explicitly).

Enter submits or queues a follow-up while work is active. Ctrl+S steers at the
next model boundary; pending steering is shown above the composer. Escape
interrupts. Ctrl+C clears a nonempty draft, otherwise aborts active work and does
nothing while idle. Ctrl+D coordinates close from any input surface, settling
active work and child-process cleanup first. Shift+Enter inserts a newline when
the terminal reports enhanced keys. [Full key table](commands.md#keys).

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

The renderer uses Pi's complete retained frame, synchronized frames, and exact
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

Reasoning is collapsed by default; Ctrl+O expands retained content. Each accepted
run begins a bold, model-adaptive shimmering `Working` row. One trailing
`Working (<elapsed> • esc to interrupt)` remains even after assistant text until
the run settles; tool admission replaces it with the tool lifecycle.

While reasoning is active, the fixed two-row status has a shimmering `Thinking`
header and the latest explicit ATX or standalone-bold Markdown heading, followed
by a subdued expansion hint. Ordinary reasoning body text is never promoted to
a label; without a heading, only the hint appears on the second row.

```text
• Thinking
  └ Verifying the implementation (ctrl+o to expand)
```

Expanded reasoning retains its inset without an event-margin dot or synthetic
first-line bullet. Completed reasoning disappears again when collapsed.
Reasoning and assistant-response dots are solid; active tool/shell dots pulse
foreground/muted tones without changing size. Completed success is green and
failed tools red. [Selecting reasoning](providers.md#reasoning).

## Tool evidence and worker activity

In terse mode, a Bash tool command shows its first three rendered lines, followed
by a count of hidden command lines. Ctrl+O expands the complete retained command
and collapses it again; this does not alter the executed command or the separate
output preview.

The transcript's **Subagents** event indents the complete worker roster beneath
its heading. Rows retain state, input/output tokens, and cost, but omit call
counts; the underlying telemetry and inspector remain unchanged.

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

Generic extension state is on demand. The subagents exception keeps an owner-scoped
complete bounded roster immediately above the composer during an owning run:
phase, tool calls, disjoint input/cache and output totals, and priced spend.
The nonblocking 250 ms refresh retains its last fenced snapshot on failure.
`/subagents` opens an arrow-key list; Enter opens a scrollable read-only child
transcript. No extension replaces the cumulative footer. Completed child usage
is mirrored once into the root ledger before settlement, including later cost
limits. [Worker presentation and accounting](../extensions/octet-subagents/README.md#tui-and-serve-presentation).
