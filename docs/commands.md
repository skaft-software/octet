# Commands and keys

[Documentation](README.md) · [CLI flags](cli.md) · [Terminal](terminal.md)

Type `/` in the input to see every command. Up and Down highlight a suggestion;
Tab inserts it without running it, even when multiple names match. This includes
`/skill:NAME` suggestions. A name typed in full stays itself (`/skill:review`
is not completed to `/skill:review-pr`) until you move the highlight. Tab only
completes a suggestion that is on screen. Enter sends the selection through its normal handler.

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
| `/tree` | Select a branch in the same session file, optionally summarizing abandoned work or supplying custom summary instructions. |
| `/fork` | Create a new session file from an earlier message, or the whole conversation. |
| `/clone` | Copy the current session at its latest point. |
| `/model [id]` | Pick a model, or select `id`. |
| `/fast [on\|off\|status]` | Turn capability-gated Responses priority on or off, or show it. A change made during a run waits for a safe point. |
| `/thinking [level]` | Show or change the [reasoning level](providers.md#reasoning). |
| `/theme [auto\|light\|dark\|name]` | Pick a built-in appearance or a discovered TOML theme. With no argument, it opens a filterable picker. |
| `/answer [instruction]` | Stop using tools at the next safe point and answer from what's gathered. |
| `/compact [instructions]` | Compact at the next safe point. Custom instructions apply to local summaries, not native Responses compaction. |
| `/verbose [on\|off]` | Expand or collapse reasoning, compaction and tool output. Ctrl+O and Ctrl+T steer the same two modes separately. |
| `/reload` | Reload your keybindings, instructions, prompts, skills and enabled extensions at a safe point. On Unix, `/reload --force` restarts the app into the executable currently on disk and resumes the same session, even if that executable is unchanged. It is also explicit consent to enter a moved or replaced binary; compatibility and durable-session checks still apply. Plain `/reload` is resources-only. Automatic host re-exec requires an executable change and `reload_host = true`. Refused while a model turn, tool call, shell child, effect approval or in-flight session write is active. Finished subagents (done, limit reached, failed, timed out) never block a reload: they are parked with their result and session kept, and `subagent_continue` resumes them afterwards. A subagent that is still running or has queued work blocks plain `/reload`; `/reload --force` detaches it instead, keeping its session and queued work. |
| `/login [provider]` | Sign in to a subscription provider. |
| `/setup` | Open the provider setup wizard at any time, to add or replace a built-in API key, sign in with a supported subscription, or set up a local endpoint, or to remove a saved key, sign-in or custom provider. During a run it waits for the idle point, and your current model and session aren't switched. |
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
| `/export [path]` | Export redacted HTML by default; `.json` selects whole-graph data and `.jsonl` native Octet JSONL. |
| `/import path` | Confirm a new private copy of supported Octet/Pi session data, then switch to it without submitting the draft. |
| `/share` | Review an immutable redacted all-branches snapshot and explicitly confirm an unlisted GitHub gist. Cancel is the default; unlisted is not private. |
| `/prompt [name] [arguments]` | List or expand prompt templates. `/<name> ...` also works, as in Pi. |
| `/skills ...` | List, search, inspect, load, unload or reload [skills](instructions.md#skills). |
| `/skill:NAME [arguments]` | Expand a skill as a user prompt when sent, not as a local slash command. During a run, Enter or Ctrl+S queues it, so it expands at the next idle prompt. |
| `/extensions [install\|status\|reload]` | Enable, disable and configure extensions (below), or show or reload extension state. `/extensions install` downloads and installs every missing first-party bundle with live progress; installed bundles stay off until you enable them. Menu changes and installs wait for idle during a run. |
| `/subagents [list\|status\|inspect\|wait\|reattach\|stop\|open-all]` | Browse and control this session's workers when octet-subagents is ready. Bare `/subagents`, `list` and `status` open the live roster, including during a run; `/subagents stop <name-or-id\|all>` requests owner-bound interruption immediately. Other operations wait for idle. |
| `/settings [show\|theme\|tui fullscreen/inline\|steering one-at-a-time/all\|follow-up one-at-a-time/all\|images on/off\|default model/reasoning\|transport\|padding]` | Open the settings menu, or use an explicit subcommand. `/settings show` reports effective settings. Defaults, theme, terminal screen, steering and follow-up modes and images persist through the shared config writer. Transport and editor padding are reported facts, and project trust is deliberately not a persisted setting. |
| `/scoped-models [all\|clear\|enable\|disable\|toggle\|move]` | Manage the ordered model-cycling scope. Changes apply to Ctrl+P at once and persist as an exact ordered pattern list (`models` in the user config). `move <id> <up\|down\|top\|bottom>` reorders it. |
| `/help [command]` | Help for a command. |
| `/exit` | Exit octet. |

Type `/log…` or `/setu…` to see both `/login` and `/setup` in the suggestions,
then pick one with Up and Down. Tab inserts the highlighted name without running
it; Enter runs it. With the popup dismissed, Tab still requires a unique match.
Exact `/login` and `/setup` keep their separate actions. `/theme` previews and
selects the built-in Auto, Light and Dark appearances, or valid TOML files in the
trusted theme discovery roots.
Cancelling leaves the theme and session unchanged ([Theme discovery and
format](themes.md)). Extensions can add commands, and each package's README
lists its arguments.

A successful `/reload --force` shows one green `reload succeeded` notice after
restarting and resuming the same session. If it cannot restart, the session stays
usable and one red `reload failed` notice names the stage and the next step.
These notices are temporary: they are not saved in the session or replayed on a
later resume.

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
silently. Capture is bounded by `max_output_bytes`; `bash_timeout_secs` imposes
an optional execution deadline. There is no timeout by default, and cancellation
still stops the command. It isn't a persistent shell session.

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

## Read-only reports

Interactive status and fact reports use section headings and bold field labels,
including cache warming, safety permissions and telemetry. Cost and cache records
stack their named fields rather than clipping wide ASCII tables. Values remain
literal text; filenames, model names and diagnostics cannot inject Markdown or
terminal controls. Plain/CLI report output keeps its text format.

Up/Down scroll, PageUp/PageDown page, and Escape returns to the unchanged draft.
Reports are transient: they do not enter transcript copy, session history or
model input, and reading a report never approves an effect.

## Settings menu

Bare `/settings` opens a filterable menu immediately, including during active
work. Choose **Theme**, **Terminal screen** (fullscreen, the default, or
inline), **Steering mode**, **Follow-up mode**, **Inline tool-result images
(On/Off)**, **Default model for new sessions**, **Default reasoning for new
sessions**, or **Show effective settings**. Up and Down select, typing filters, and Enter opens or applies the
highlighted choice. Child menus offer **Back**; Escape closes the menu without
changing a preference.

Theme uses the existing preview picker. Inline images offers explicit On and Off
choices for tool-result display only, not upload or attachment consent. Default
model choices come from the available credential-scoped catalog; default
reasoning choices use available portable levels. Saving either default does not
switch the active session: use `/model` or `/thinking` for that. The menu does not
label the current session or launch selection as a saved default. Selected
changes use the same safe display or deferred settings dispatch as explicit
commands; opening and browsing the menu does not interrupt active work.

**Steering mode** and **Follow-up mode** are Pi's `steeringMode` and
`followUpMode` (config keys `steering_mode` and `follow_up_mode`). Both default to
`one-at-a-time`, as in Pi. Steering messages are sent while the agent works:
`one-at-a-time` delivers one per turn boundary, `all` delivers every queued
message at the next boundary. Follow-ups wait until the agent finishes:
`one-at-a-time` runs each as its own turn, `all` sends consecutive queued prompts
together in one turn (slash commands, `!` shell commands and `/answer` still run
on their own). A change applies from the next run. **Terminal screen** chooses
fullscreen (no scrollback replay or flicker) or the inline, differential renderer;
`--tui-mode` overrides it for one launch.

**Show effective settings**, or `/settings show`, opens read-only diagnostics.
**Configured model** is a launch fact and **Active reasoning** is session state;
neither is presented as the saved default for new sessions. In narrower terminal
panes, settings, extension and setup menus put descriptions below their bold
action labels instead of squeezing both into one row. Native Tern sheets keep
bold action labels in the list and wrap the full selected description below it.
Explicit `/settings theme`, `tui`, `steering`, `follow-up`, `images`, `default
model`, `default reasoning`, `transport` and `padding` forms are also available. Transport,
padding, permissions and project trust are not editable menu preferences, and
the menu adds no cache-policy controls.

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
| Shift+Enter / Ctrl+J | New line. Shift+Enter requires a [distinguishable terminal encoding](terminal.md#shiftenter-terminal-support); otherwise it may submit. |
| Shift+Tab | Cycle through the active model's thinking levels, lowest to highest, wrapping at the end. During a run it updates the queued selection at once. |
| Ctrl+S | Queue an editable follow-up for after the run; send normally when idle. In the resume picker, change the sort order. |
| Escape | Close a panel or slash popup first. Otherwise interrupt active work, then send the oldest queued follow-up after it settles (never the draft). |
| Option+Up / Alt+Up | Recall the newest editable queued steering message or follow-up into an empty input. It never submits or interrupts. On Windows and WSL, where Windows Terminal uses Alt+arrows for panes, use Alt+Q. The queued-message hint names the key in use. |
| Ctrl+C | Clear a draft. With no draft, abort active work; when idle, close octet. |
| Ctrl+Shift+C | Copy the in-app transcript selection (also copied when a drag is released). |
| Ctrl+Shift+A | Select the semantic transcript; focused panels retain their own input. |
| Ctrl+D | Close octet from any input, after active work and child processes are cleaned up. |
| Ctrl+P / Ctrl+Shift+P | Cycle models within the resolved `--models` scope (or the available catalog with no scope). Backward is Alt+P on Windows and WSL. Drafts are kept. |
| Ctrl+L | Open the model selector. |
| Ctrl+Up / Ctrl+Down | Jump between prompt boundaries. |
| Ctrl+O | Show compaction, subagent activity, tool commands, tool evidence and shell output. It can't recover discarded output. |
| Ctrl+T | Show or hide the retained thinking traces, independently of Ctrl+O. New traces follow the current choice. |
| PageUp / PageDown | Scroll the transcript. PageUp pins the view, PageDown returns toward live output. |
| Up / Down | Pick a visible suggestion first, otherwise move through multiline input. At the first/last editor row, recall sent prompts and return to the exact draft, caret and paste/attachment chips—even during active work. History never submits. |
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
