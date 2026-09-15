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

Commands run immediately when safe. Model/reasoning/session changes, compaction,
reload, and checkout queue until active work releases ownership. `/answer`
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
| `/tree` | Show the complete durable conversation branch tree. |
| `/checkout <id>` | Move the durable head to another entry and branch without deleting ancestry. |
| `/model [id]` | Open the model picker or select an ID. |
| `/thinking [level]` | Inspect/change [model-supported reasoning](providers.md#reasoning). |
| `/theme [auto\|light\|dark]` | Choose the compiled terminal appearance or open its picker. |
| `/answer [instruction]` | Stop tool use at the next safe boundary and answer from gathered evidence. |
| `/compact` | Request compaction at the next safe boundary. |
| `/verbose [on\|off]` | Expand/collapse retained reasoning, compaction, and bounded tool evidence. |
| `/reload` | Reload instructions, prompts, skills, and enabled extensions at a safe boundary. |
| `/login [provider]` | Sign in to a subscription provider. |
| `/logout [provider]` | Remove its stored credential. |
| `/status` | Active model, context, capabilities, and diagnostics. |
| `/context` | Context composition and effective capacity. |
| `/cost` | Turn/session usage and cost accounting, including durable delegated spend. |
| `/cache` | Provider-reported prompt-cache diagnostics. |
| `/changelog` | Read the current version's bundled release notes in a scrollable rich-Markdown TUI report; no network or model request. |
| `/update` | Check for a newer release; install with `octet update`, subject to [channel availability](installation.md#binary-availability). |
| `/name [name]` | Show or rename the current session. |
| `/export [path]` | Export the current session with redaction. |
| `/prompt [name] [arguments]` | List/expand named templates; Pi-compatible `/<name> ...` invocation is also supported. |
| `/skills ...` | List, search, inspect, load, unload, or reload skills; [activation](instructions.md#skills). |
| `/extensions [status\|reload]` | Open installed-bundle enable/disable menu or inspect/reload state. |
| `/subagents` | With the trusted, enabled subagents package live, browse workers and read-only transcripts. |
| `/help [command]` | Local command help and self-documentation. |
| `/quit` | Exit octet. |

`/theme` changes only the compiled terminal appearance selector; it does not
load arbitrary theme files. [Theme status](themes.md).
Additional extension commands depend on the enabled, independently trusted
package; its README is authoritative for arguments.

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
| Option+Up / Alt+Up | Move the newest queued follow-up into an empty composer for editing; no submission or interruption. |
| Ctrl+C | Clear a nonempty draft; otherwise abort active work, no-op while idle. |
| Ctrl+D | Close from any interactive input surface after active-work and child-process cleanup. |
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
Ctrl+S is different: it admits live steering at the next model boundary and
cannot be retracted with Option+Up. Failed runs, Ctrl+C cancellation, and close
do not auto-dispatch follow-ups; retained entries can still be recalled while
idle. Option+Up never overwrites a nonempty draft. Attachment/paste chips retain
their payloads when recalled. Held-key repeats do not submit, interrupt, or pop
queue entries.
