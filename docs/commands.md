# Commands and keys

[Documentation](README.md) · [CLI flags](cli.md) · [Terminal](terminal.md)

Type `/` in the composer for live command discovery. For example:

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
| Enter | Submit; while active, queue a follow-up. In a picker, select the visible action. |
| Shift+Enter | Newline when enhanced terminal key events are available. |
| Ctrl+S | Steer at the next model boundary; in the resume picker, cycle sorting. |
| Escape | Interrupt active work; close/back out of a panel. |
| Ctrl+C | Clear a nonempty draft; otherwise abort active work, no-op while idle. |
| Ctrl+D | Close from any interactive input surface after active-work and child-process cleanup. |
| Ctrl+O | Globally disclose retained reasoning, compaction, delegated activity, tool commands, tool evidence, and shell output. Cannot recover discarded capture bytes. |
| PageUp / PageDown | Semantic transcript navigation; PageUp claims the bounded viewport, PageDown returns toward live output. |
| Up / Down | Select a path or `@` file suggestion while its menu is visible; otherwise move through the editor. |
| Tab | Insert the selected path or `@` file suggestion. Keep directory completion open; backslash-escape spaces. |
| `@` | Fuzzy gitignore-aware workspace file mentions; path-prefixed mentions use filesystem completion. |

[Resume picker keys](sessions.md#resume-and-branch) and
[subagent navigation](../extensions/octet-subagents/README.md#tui-and-serve-presentation)
are contextual. Keyboard ownership and native mouse history are explained in
[terminal scrolling](terminal.md#scrolling-and-rendering).
