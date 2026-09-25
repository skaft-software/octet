# Commands and keys

[Documentation](README.md) · [CLI flags](cli.md) · [Terminal](terminal.md)

Type `/` in the composer for live command discovery. Tab completes a
slash-command name without invoking it; Enter invokes the highlighted command
through its normal dispatcher. For example:

```text
/status
/model
/answer Answer from the evidence already gathered. Do not use more tools.
```

Commands run immediately when safe. Model/session changes, compaction, reload,
and checkout queue until active work releases ownership. `/thinking` uses a
noninterrupting next-response control on explicitly qualified Responses routes;
other routes retain the idle-boundary selector. Queued effort is not provider
acknowledgement. Unsupported explicit efforts are rejected. `/answer`
persists a steering instruction and makes subsequent requests tool-free at the
next safe boundary; while idle it starts a tool-free run. It is not an undo for
already admitted effects. [Run control contract](design/octet-agent.md#commit-and-cancellation-invariants).

## Slash commands

| Command | Purpose |
| --- | --- |
| `/new` | Start a fresh conversation. |
| `/resume [id]` | Open the session picker or resume an ID. |
| `/fork` | Fork from an active-branch user message or the whole conversation. |
| `/clone` | Clone the current session at its active head. |
| `/model [id]` | Open the model picker or select an ID. |
| `/fast [on\|off\|status]` | Toggle/inspect capability-gated Responses priority; active changes wait for a safe boundary. |
| `/thinking [level]` | Inspect/change [model-supported reasoning](providers.md#reasoning). |
| `/theme [auto\|light\|dark]` | Choose the terminal appearance; without an argument, open the picker. |
| `/answer [instruction]` | Stop tool use at the next safe boundary and answer from gathered evidence. |
| `/compact [instructions]` | Request compaction at the next safe boundary; bounded custom instructions apply to local summaries, not native Responses compact. |
| `/verbose [on\|off]` | Expand/collapse retained reasoning, compaction, and bounded tool evidence. |
| `/reload` | Reload user keybindings, instructions, prompts, skills, and enabled extensions at a safe boundary. Host re-exec is **opt-in**: it happens only on `/reload --force` (or an executable change with `reload_host = true`), and a moved/replaced binary is confirmed before it is probed. Refused while a model turn, tool call, shell child, effect approval, in-flight session write, or delegated worker is active. |
| `/login [provider]` | Sign in to a subscription provider. |
| `/setup` | Open the provider setup wizard at any time to add/replace a built-in API key, sign in with a supported subscription, or configure a local endpoint. While a run is active, waits for the idle boundary; the current model/session are not switched. |
| `/logout [provider]` | Remove its stored credential. |
| `/status` | Active model, context, capabilities, and diagnostics. |
| `/session [info]` | Read-only session identity, file, branch, checkpoints and accounting. |
| `/hotkeys` | Inspect the resolved keyboard bindings. |
| `/copy` | Copy the last assistant message through the existing text clipboard consumer. |
| `/context` | Context composition and effective capacity. |
| `/cost` | Turn/session usage and cost accounting, including durable delegated spend. |
| `/cache` | Provider-reported prompt-cache diagnostics. |
| `/changelog` | Read the current version's bundled release notes in a scrollable rich-Markdown TUI report; no network or model request. |
| `/update` | Check for a newer release; install with `octet update`, subject to [channel availability](installation.md#binary-availability). |
| `/name [name]` | Show or rename the current session. |
| `/export [path]` | Export the current session with redaction. |
| `/prompt [name] [arguments]` | List/expand named templates; Pi-compatible `/<name> ...` invocation is also supported. |
| `/skills ...` | List, search, inspect, load, unload, or reload skills; [activation](instructions.md#skills). |
| `/skill:NAME [arguments]` | Expand an explicit skill as a user prompt at admission, not a local slash command. During a run, Enter or Ctrl+S queues a follow-up so expansion occurs at the next idle prompt boundary. |
| `/extensions [status\|reload]` | Open installed-bundle enable/disable menu or inspect/reload state. |
| `/settings [theme\|images on/off\|default model/reasoning\|transport\|padding]` | Show or change user-level display/default preferences. Defaults, theme, and images persist through the shared config writer; transport and editor padding are reported route/theme facts, and project trust is deliberately not a persisted setting. |
| `/scoped-models [all\|clear\|enable\|disable\|toggle\|move]` | Manage the ordered model cycling scope. Mutations apply to Ctrl+P immediately and persist as an exact ordered pattern list (`models` in the user config); `move <id> <up\|down\|top\|bottom>` reorders it. |
| `/subagents` | With the trusted, enabled subagents package live, browse workers and read-only transcripts. |
| `/help [command]` | Local command help and self-documentation. |
| `/exit` | Exit octet. |

Type `/log…` or `/setu…` in the composer to see both `/login` and
`/setup` in command suggestions; choose the intended command with Up/Down and
Enter. An ambiguous partial match does not Tab-complete automatically. Exact
`/login` and `/setup` retain their separate actions.

Automatic reloads do not add success summaries, startup banners, or debounce
bookkeeping to the transcript. Host, worker-deferral, extension, provider-catalog,
and watch-limit problems are reported on appearance or change, and again if a
successful check clears them before they recur. Skipping a component does not
clear its remembered problem. Resource/bootstrap and keybinding checks use the
same component-scoped recurrence rule. Actual work-loss events are always reported.
Explicit `/reload` commands still show current results.
`/reload --dry-run` shows watch counts, timing, host policy, and possible
interruptions without applying changes. Automatic passes never open confirmation
pickers: a replacement binary or worker detach requiring consent is deferred to
`/reload --force`. Explicit command consent checks remain in place.

### Local shell escapes

A draft beginning with `!` is a **local** command, not model input: `!command`
runs it through the product process gates, the ordinary process approval, and
the bounded capture/cleanup path, then adds the result to model context as a
user message. `!!command` takes the same path but durably records the result
**excluded from model context**. Both work while idle and during a running turn
(mid-run results are appended at the next idle boundary, after any pending tool
call settles, so a call is never separated from its result). `--no-process` or
`--no-shell` disables the escape; a denial is reported, never silently dropped.
The capture is bounded by `max_output_bytes` and `bash_timeout_secs`; it is not
a persistent shell session.

`/theme` selects a built-in Auto/Light/Dark appearance. This command does not
load arbitrary theme files. [Theme status](themes.md).
Additional extension commands depend on the enabled, independently trusted
package; its README is authoritative for arguments.

## Priority and compaction boundaries

Bare `/fast` toggles priority; `/fast status` distinguishes current and queued
selection. Unsupported routes refuse rather than silently ignoring the switch.
Selection survives compatible same-session rebuilds, but resets on another
session or process restart; it is not a persisted preference. Enabling it first
records durable priority-cost uncertainty. Disabling it does not erase that
exposure, and hard cost ceilings remain fail-closed. The default 272K Codex
context cap is unchanged. Selection is not proof that a provider granted priority.

Custom `/compact` instructions are at most 16 KiB and reject unsupported control
characters. Native Responses compaction explicitly refuses custom instructions.
Local main/split-prefix summaries use host-owned retries, reservations and
accounting; the summary is committed once. This does not promise retry-progress
UI or exactly-once inference after a transport interruption.

## Extension activation menu

`/extensions` lists managed executable bundles, not the separate Serve
application. Up/Down selects; Enter enables/disables only the selected user
`enabled_extensions` entry. It never writes a trust grant: full access implicitly
trusts enabled extensions, while safe mode keeps executable extensions stopped.
Selecting enabled `octet-web-search` opens the provider picker: Brave Search is recommended,
SearXNG remains optional, and Brave's key is requested through private input.
[Web-search setup](../extensions/octet-web-search/README.md#choose-a-provider).

Project/environment/CLI activation makes the menu read-only with the source
boundary shown. Shadowed or alternate sources cannot be silently substituted.
Unavailable enabled bundles remain disable-only; enabling fails closed when
one-shot/alternate-source trust could change the selected executable, and
explicit required-tool removal blocks disabling. Safe mode keeps all executable
extension processes stopped. [Discovery and trust](resources.md).

## Keys

| Key | Action |
| --- | --- |
| Enter | Submit; while active, queue an editable follow-up for after the run. In a picker, select the visible action; in the slash-command popup, invoke the highlighted command. |
| Shift+Enter | Newline when enhanced terminal key events are available. |
| Ctrl+S | Steer at the next model boundary; in the resume picker, cycle sorting. |
| Escape | Interrupt active work, then dispatch the oldest queued follow-up after settlement (never the draft); close/back out of a panel or slash popup first. |
| Option+Up / Alt+Up | Recall the newest editable queued steering message or follow-up into an empty composer; no submission or interruption. |
| Ctrl+C | Clear a nonempty draft; otherwise abort active work, no-op while idle. |
| Ctrl+D | Close from any interactive input surface after active-work and child-process cleanup. |
| Ctrl+P / Ctrl+Shift+P | Cycle models within the available resolved `--models` scope (or the available catalog without a scope); backward is Alt+P on Windows/WSL. Drafts are preserved. |
| Ctrl+L | Open the model selector. |
| Ctrl+Up / Ctrl+Down | Jump between semantic prompt boundaries. |
| Ctrl+O | Globally disclose retained reasoning, compaction, delegated activity, tool commands, tool evidence, and shell output. Cannot recover discarded capture bytes. |
| PageUp / PageDown | Semantic transcript navigation; PageUp claims the bounded viewport, PageDown returns toward live output. |
| Up / Down | Select a visible path/`@` suggestion; otherwise move through the editor. While idle, Up at cursor 0 recalls sent prompts; Down past the newest restores the draft. |
| Tab | Complete the selected slash command without invoking it, or insert the selected path/`@` file suggestion. Keep directory completion open; backslash-escape spaces. |
| `@` | Fuzzy gitignore-aware workspace file mentions; path-prefixed mentions use filesystem completion. |

[Resume picker keys](sessions.md#resume-and-branch) and
[subagent navigation](../extensions/octet-subagents/README.md#tui-and-serve-presentation)
are contextual. Keyboard ownership and native mouse history are explained in
[terminal scrolling](terminal.md#scrolling-and-rendering).

Follow-ups remain local until dispatch, one per settled run in FIFO order.
Ctrl+S is different: it queues live steering for the next model boundary.
Option+Up can retract it until the agent claims it for persistence; once that
boundary is crossed it cannot be recalled, even before the UI displays delivery.
The newest eligible steering message or follow-up is recalled, preserving its
attachment/paste chips. `/answer` is not retractable because it changes the run's
tool policy. Failed runs, Ctrl+C cancellation, and close do not auto-dispatch
follow-ups; retained entries can still be recalled while idle. Option+Up never
overwrites a nonempty draft. Held-key repeats do not submit, interrupt, or pop
queue entries.

## User keybindings and transcript search

The interactive shell loads `~/.octet/keybindings.json`; `/reload` reloads it and
`/hotkeys` shows the resolved platform bindings. Invalid or conflicting bindings
are diagnosed, not silently granted executable authority. Bindings do not add
clipboard image permission or chord handling.

Ctrl+Shift+F (Ctrl+F on Windows/WSL) opens bounded rendered-transcript search in
the primary-screen viewport. Enter/Ctrl+G selects the next match;
Shift+Enter/Ctrl+Shift+G selects the previous one; Escape closes search. Search
owns its query input, including paste, rather than submitting it or admitting
attachments. It does not search discarded capture bytes or native terminal
scrollback. The resume picker's Ctrl+F separately searches bounded session text.
Ctrl+Shift+B cycles hidden/auto/always transcript scrollbar modes for this shell;
this is not a persisted preference or alternate-screen switch.
