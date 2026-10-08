# Commands and keys

[Documentation](README.md) · [CLI flags](cli.md) · [Terminal](terminal.md)

Type `/` in the input to see every command. Tab completes a command name without
running it, and Enter runs the highlighted command through its normal handler.

```text
/status
/model
/answer Answer from the evidence already gathered. Do not use more tools.
```

Most commands run right away. Changes to the model or session, compaction,
reload and checkout wait until current work finishes, and a change requested
while work is active is listed in the queued-input area until it applies.
`/thinking` takes effect without interrupting on explicitly qualified Responses
routes, and other routes change at an idle boundary. A queued effort isn't a
provider acknowledgement, and unsupported explicit efforts are rejected.
Extensions queue the same way through `pi.setModel` and `pi.setThinkingLevel`.
`/answer` saves a steering instruction and makes later requests tool-free at
the next safe point, or starts a tool-free run when idle. It doesn't undo
effects that already happened. [Run
control](design/octet-agent.md#commit-and-cancellation-invariants).

## Slash commands

| Command | What it does |
| --- | --- |
| `/new` | Start a new conversation. |
| `/resume [id]` | Pick a session, or resume `id`. |
| `/fork` | Branch from an earlier message, or the whole conversation. |
| `/clone` | Copy the current session at its latest point. |
| `/model [id]` | Pick a model, or select `id`. |
| `/fast [on\|off\|status]` | Turn capability-gated Responses priority on or off, or show it. A change made during a run waits for a safe point. |
| `/thinking [level]` | Show or change the [reasoning level](providers.md#reasoning). |
| `/theme [auto\|light\|dark\|name]` | Pick a built-in appearance or a discovered TOML theme. With no argument, it opens a filterable picker. |
| `/answer [instruction]` | Stop using tools at the next safe point and answer from what's gathered. |
| `/compact [instructions]` | Compact at the next safe point. Custom instructions apply to local summaries, not native Responses compaction. |
| `/verbose [on\|off]` | Expand or collapse reasoning, compaction and tool output. |
| `/reload` | Reload your keybindings, instructions, prompts, skills and enabled extensions at a safe point. Host re-exec is **opt-in**: only `/reload --force` (or an executable change with `reload_host = true`) triggers it, and a moved or replaced binary is confirmed before it's probed. Refused while a model turn, tool call, shell child, effect approval, in-flight session write or subagent is active. |
| `/login [provider]` | Sign in to a subscription provider. |
| `/setup` | Open the provider setup wizard at any time, to add or replace a built-in API key, sign in with a supported subscription, or set up a local endpoint. During a run it waits for the idle point, and your current model and session aren't switched. |
| `/logout [provider]` | Remove its stored credential. |
| `/status` | Active model, context, capabilities and diagnostics. |
| `/session [info]` | Read-only session identity, file, branch, checkpoints and accounting. |
| `/goal [objective\|status\|pause\|resume\|clear]` | Set, read, pause, resume or clear the durable session goal. `/goal` applies at once, mid-run or idle, and is never queued; if the run cannot address the goal store it fails closed with a visible reason. |
| `/hotkeys` | Show the resolved keyboard bindings. |
| `/copy` | Copy the last assistant message through the text clipboard. |
| `/context` | What's in the context, and its effective capacity. |
| `/cost` | Usage and cost for the turn and session, including subagents. |
| `/cache` | Prompt-cache details from the provider. |
| `/changelog` | Read this version's bundled release notes in a scrollable report. No network or model request. |
| `/update` | Check for a newer release. Install it with `octet update`, subject to [channel availability](installation.md#binary-availability). |
| `/name [name]` | Show or rename the session. |
| `/export [path]` | Export the session with secrets redacted. |
| `/prompt [name] [arguments]` | List or expand prompt templates. `/<name> ...` also works, as in Pi. |
| `/skills ...` | List, search, inspect, load, unload or reload [skills](instructions.md#skills). |
| `/skill:NAME [arguments]` | Expand a skill as a user prompt when sent, not as a local slash command. During a run, Enter or Ctrl+S queues it, so it expands at the next idle prompt. |
| `/extensions [status\|reload]` | Enable, disable and configure extensions (below), or show or reload extension state. Menu changes wait for idle during a run. |
| `/subagents [list\|status\|inspect\|wait\|reattach\|stop\|open-all]` | Browse and control this session's workers when octet-subagents is ready. Bare `/subagents`, `list` and `status` open the live roster, including during a run; `/subagents stop <name-or-id\|all>` requests owner-bound interruption immediately. Other operations wait for idle. |
| `/settings [theme\|images on/off\|default model/reasoning\|transport\|padding]` | Show or change user-level display and default preferences. Defaults, theme and images persist through the shared config writer. Transport and editor padding are reported facts, and project trust is deliberately not a persisted setting. |
| `/scoped-models [all\|clear\|enable\|disable\|toggle\|move]` | Manage the ordered model-cycling scope. Changes apply to Ctrl+P at once and persist as an exact ordered pattern list (`models` in the user config). `move <id> <up\|down\|top\|bottom>` reorders it. |
| `/help [command]` | Help for a command. |
| `/exit` | Exit octet. |

Type `/log…` or `/setu…` to see both `/login` and `/setup` in the suggestions,
then pick one with Up and Down and Enter. An ambiguous partial match doesn't
Tab-complete on its own, and exact `/login` and `/setup` keep their separate
actions. `/theme` previews and selects the built-in Auto, Light and Dark
appearances, or valid TOML files in the trusted theme discovery roots.
Cancelling leaves the theme and session unchanged ([Theme discovery and
format](themes.md)). Extensions can add commands, and each package's README
lists its arguments.

<details>
<summary>How automatic reloads report</summary>

Automatic reloads don't add success summaries, startup banners or debounce notes
to the transcript. Host, worker-deferral, extension, provider-catalog and
watch-limit problems are reported when they appear or change, and again if a
successful check clears them before they recur. A skipped component doesn't
clear its remembered problem. Resource, bootstrap and keybinding checks use the
same per-component rule. Real work-loss events are always reported, and an
explicit `/reload` still shows current results. `/reload --dry-run` shows watch
counts, timing, host policy and possible interruptions without applying
anything. Automatic passes never open confirmation pickers: a replacement
binary, or a worker detach that needs consent, is deferred to `/reload --force`.

</details>

### Local shell escapes

A draft that starts with `!` is a **local** command, not model input. `!command`
runs through the product's process gates, the ordinary process approval and the
bounded capture and cleanup path, then adds the result to the model's context as
a user message. `!!command` takes the same path but durably records the result
**excluded from model context**. Both work while idle and during a run. A
mid-run result is appended at the next idle point, after any pending tool call
settles, so a call is never separated from its result. `--no-process` or
`--no-shell` turns the escape off, and a denial is reported, never dropped
silently. Capture is bounded by `max_output_bytes` and `bash_timeout_secs`, and
it isn't a persistent shell session.

## Priority and compaction boundaries

Bare `/fast` toggles priority, and `/fast status` shows the current and queued
choice. Unsupported routes refuse instead of silently ignoring the switch. The
choice survives compatible same-session rebuilds but resets on another session
or a process restart, so it isn't a saved preference. Turning it on first
records durable priority-cost uncertainty. Turning it off doesn't erase that
exposure, so hard cost ceilings stay fail-closed. The default 272K Codex context
cap is unchanged, and selecting priority isn't proof that a provider granted it.

Custom `/compact` instructions are at most 16 KiB and reject unsupported control
characters. Native Responses compaction refuses them outright. Local main and
split-prefix summaries use host-owned retries, reservations and accounting, and
the summary is committed once. That doesn't promise retry-progress UI or
exactly-once inference after a transport interruption.

## Extension menu

`/extensions` is the one place to turn extensions on and set them up. It lists
installed executable bundles, plus any extension loaded from `--extension-dir`
or the project (whose activation changes where it came from). Up and Down
select. Enter opens the selected extension's
options, enabling an installed one first if it's disabled. Each menu shows the
extension's state and offers only what applies:

- **octet-computer-use:** Set up computer use, Check status, Jev (optional) and
  the jev-use recipe.
- **octet-web-search:** Use Brave Search (recommended), Use SearXNG, Change the
  SearXNG endpoint and Log out of Brave Search.
- **octet-subagents:** Enable or disable orchestration. Use `/subagents` for
  the live worker roster, inspection, wait, reattach and stop controls; these
  runtime operations are not extension configuration.
- **octet-mcp:** Add a server, then per server Show details, Refresh tools,
  Restart or Start, Stop, Enable or Disable, Edit and Remove.

Every menu also offers **Disable**. An action shows its steps live while it runs
(Esc cancels), and its prompts (confirmations, choices, keys entered as hidden
input) appear in place. Actions an extension marks destructive ask first. A
third-party extension without its own menu gets one entry per command it
declares, which asks for that command's arguments.

Commands published by ready extensions appear in slash completion and execute
through their registered handlers. During active work, commands that need an
idle boundary are visibly queued. `/extensions` remains the configuration menu;
`/subagents` remains a runtime slash command while its first-party extension is
ready.
The menu never writes a trust
grant on enablement. Full access implicitly trusts enabled extensions; safe
mode starts only enabled extensions with explicit source-bound host authority.

<details>
<summary>When the menu is read-only or blocked</summary>

- An extension enabled from the project, the environment or the command line
  shows where it came from, and can't be changed here. A shadowed or alternate
  source is never swapped in silently.
- An enabled bundle that's unavailable can only be disabled.
- Enabling fails if a one-shot or alternate-source trust could change which
  executable runs.
- Disabling is blocked if it would remove a tool you've explicitly required.
- Safe mode keeps every extension process stopped.

See [Discovery and trust](resources.md).

</details>

## Keys

| Key | What it does |
| --- | --- |
| Enter | Send when idle; while octet works, steer at the next model boundary. In a picker, choose the highlighted action. In the slash popup, run the highlighted command. |
| Shift+Enter | New line, if your terminal reports enhanced key events. |
| Shift+Tab | Cycle through the active model's thinking levels, lowest to highest, wrapping at the end. During a run it updates the queued selection at once. |
| Ctrl+S | Queue an editable follow-up for after the run; send normally when idle. In the resume picker, change the sort order. |
| Escape | Close a panel or slash popup first. Otherwise interrupt active work, then send the oldest queued follow-up after it settles (never the draft). |
| Option+Up / Alt+Up | Recall the newest editable queued steering message or follow-up into an empty input. It never submits or interrupts. On Windows and WSL, where Windows Terminal uses Alt+arrows for panes, use Alt+Q. The queued-message hint names the key in use. |
| Ctrl+C | Clear a draft. With no draft, abort active work. Does nothing when idle. |
| Ctrl+Shift+C | Copy the in-app transcript selection (also copied when a drag is released). |
| Ctrl+Shift+A | Select the semantic transcript; focused panels retain their own input. |
| Ctrl+D | Close octet from any input, after active work and child processes are cleaned up. |
| Ctrl+P / Ctrl+Shift+P | Cycle models within the resolved `--models` scope (or the available catalog with no scope). Backward is Alt+P on Windows and WSL. Drafts are kept. |
| Ctrl+L | Open the model selector. |
| Ctrl+Up / Ctrl+Down | Jump between prompt boundaries. |
| Ctrl+O | Show kept reasoning, compaction, subagent activity, tool commands, tool evidence and shell output. It can't recover discarded output. |
| PageUp / PageDown | Scroll the transcript. PageUp pins the view, PageDown returns toward live output. |
| Up / Down | Pick a visible path or `@` suggestion, otherwise move through the editor. When idle, Up at position 0 recalls sent prompts and Down past the newest restores your draft. |
| Tab | Complete the selected slash command without running it, or insert the selected path or `@` file suggestion. Directories stay open, and spaces are backslash-escaped. |
| `@` | Fuzzy-find workspace files, respecting `.gitignore`. With a path prefix, it completes from the filesystem. |

The [resume picker](sessions.md#resume-and-branch) and the [subagent worker
list](../extensions/octet-subagents/README.md#tui-and-serve-presentation)
(Ctrl+X stops the selected worker) have their own keys. [Terminal
scrolling](terminal.md#scrolling-and-rendering) covers the mouse.

<details>
<summary>How queued follow-ups and steering behave</summary>

Ctrl+S follow-ups stay local until sent, one per settled run, in order. Enter
during active work instead queues live steering for the next model boundary.
Option+Up can retract steering until the agent claims it for saving. Past that point it can't be
recalled, even before the UI shows delivery. The newest eligible steering
message or follow-up is recalled, with its attachment and paste chips. `/answer`
can't be retracted because it changes the run's tool policy. Failed runs, Ctrl+C
cancels and closing don't send follow-ups automatically, but retained entries
can still be recalled while idle. Option+Up never overwrites a draft, and
held-key repeats never submit, interrupt or pop queue entries.

</details>

## User keybindings and transcript search

The interactive shell loads `~/.octet/keybindings.json`. `/reload` reloads it
and `/hotkeys` shows the resolved platform bindings. Invalid or conflicting
bindings are reported, never silently given executable authority. Bindings don't
add clipboard image permission or chord handling.

Ctrl+Shift+F (Ctrl+F on Windows and WSL) opens bounded transcript search in the
primary-screen viewport. Enter or Ctrl+G selects the next match, Shift+Enter or
Ctrl+Shift+G the previous one, and Escape closes search. Search owns its query
input, including paste, and never submits it or admits attachments. It doesn't
search discarded capture bytes or your terminal's native scrollback. The resume
picker's Ctrl+F separately searches bounded session text. Ctrl+Shift+B cycles
the hidden, auto and always transcript scrollbar modes for this shell, which
isn't a saved preference or an alternate-screen switch.
