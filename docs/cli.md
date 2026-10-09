# CLI reference

[Documentation](README.md) · [Getting started](getting-started.md) · [Slash commands](commands.md)

```sh
octet --safe-mode --model claude-sonnet-4-6
octet -p "Explain the code" --tools read
```

Uppercase words are values you supply, and `[brackets]` are optional. This page
comes from the source, not from captured `--help` output. For exact parser
behavior run `octet --help`, `octet sessions --help` or
`octet migrate pi --help`. Nested-command choices, generated extension flags and
defaults that aren't listed here aren't guessed.

<a id="frontend-model-and-workspace"></a>

## Run options

| Option | What it does |
| --- | --- |
| `-p`, `--print` PROMPT | Print the final answer to stdout. Tools stay available. |
| `--mode rpc` | Pi-compatible JSONL automation. Can't be combined with `--print`. Separate from [native-host protocol 1](sdk.md) and extension API 0.4. [Limits](terminal.md#choose-a-frontend). |
| `--plain` | Chronological output, no cursor control. |
| `--color VALUE` | Color mode, such as `auto`. Terminal capability fallbacks still apply. |
| `--mouse auto\|terminal\|off\|app` | Default `auto`: native scrollback and selection without mouse capture. Explicit `app` opts into in-app wheel scrolling and drag selection. |
| `--tern auto\|on\|off` | Default `auto`: octet draws native Tern surfaces only inside a Tern pane. `on` forces negotiation anywhere, `off` always uses the terminal renderer. |
| `--show-reasoning` | Show reasoning instead of the collapsed default. |
| `--show-images` | Show tool-result images inline on compatible terminals. Off by default. It isn't upload permission or attachment consent. [Behavior](terminal.md#tool-evidence-and-worker-activity). |
| `--theme NAME` | Pick the built-in `auto`, `light` or `dark`, or a discovered TOML theme by file stem. [Theme discovery](themes.md). |
| `--model ID` | Choose the model. Overrides a resumed session's model. |
| `--reasoning LEVEL`, `--reasoning budget=N` | Reasoning effort, or a token budget where supported. [Levels](providers.md#reasoning). |
| `--cache-retention VALUE` | Provider cache retention, such as `short`. |
| `--cache-warming off\|streaming\|idle` | Billable prompt-cache refresh policy; default `streaming`. Overrides `OCTET_CACHE_WARMING` and user configuration. [Details](cache-warming.md). |
| `--max-turns N` | Limit model turns. |
| `--workspace PATH` | Workspace root for relative tool paths and the default bash directory. |
| `--workspace-trusted`, `--trust-workspace` | Load project config, instructions and resources. Can't relax global safety limits or grant extension trust. |
| `--no-context-files` | Don't compose context files. |
| `--offline` | Skip optional discovery and the background models.dev metadata refresh, and turn off remote media reads. Inference can still use the network. |
| `--strict-config` | Unknown config keys are errors, not warnings. Same as `strict_config = true`. |

See also [Terminal](terminal.md), [Models](providers.md) and
[Settings](configuration.md#settings).

RPC assistant messages use the completed response's settled cost, not a fresh
calculation from the current catalog. Their `usage.cost` is `null` when pricing
is unknown, and a known zero is different. Known total-dollar projections
include the sub-microdollar remainder, and the exact integer cost stays in the
session ledger. Aggregate scalar costs are known subtotals whenever usage is
uncertain or an operation is unpriced.

`/cache-warming` reports mode, scheduler/economic decisions and known refresh
spend; `/cache-warming MODE` persists the user setting. The local command never
becomes model input. In print/plain it reports on stderr. RPC `get_state` and
`get_session_stats` expose `cacheWarmingMode` and `cacheWarmingStatus`;
`get_state` also exposes `showCacheMissNotices`. RPC maintenance emits
`cache_warmed` with `usage`, exact integer `cost`, and `extensionOverride`,
without assistant-message or turn events. Its usage is session-only.

## Tools and limits

| Option | What it does |
| --- | --- |
| `--tools NAMES`, `--exclude-tools NAMES` | Final comma-separated allowlist or exclusions, such as `read,bash`. |
| `--powershell` | Opt in to the Windows `powershell` tool, in addition to `bash`. It never replaces `bash`, can't be combined with an exclusive `--tools` or `--no-tools` list, and reports itself inert on hosts without PowerShell. |
| `--models PATTERNS` | Ordered, comma-separated model scope for selection and Ctrl+P cycling: `provider/*`, a literal `provider/model`, or a bare-id glob, each with an optional real `:level` suffix. The first requested match is the default for a new session, and a miss warns without discarding the rest. `/scoped-models` saves the same ordered patterns. |
| `--no-tools` | Disable all tools. Can't be combined with `--tools`. |
| `--no-edit` | Disable `edit` and `write`. |
| `--no-write` | Disable whole-file `write`. |
| `--no-process`, `--no-shell` | No commands. The two are equivalent. |
| `--allow-shell` | Turn on the shell capability. It doesn't bypass separate process or effect gates. |
| `--effect-policy controlled_bash_approval\|controlled\|unsafe_host` | Set [effect admission](tools.md#authority-profiles). Default `unsafe_host`. |
| `--safe-mode` | Approval for every bash call and file change. Forces external paths off and removes the implicit authority that starts enabled extensions, so an ungranted extension stays stopped while an explicitly granted one still starts. Can't be combined with `--effect-policy`. |
| `--shell-path PATH` | Use this Bash-compatible shell. `$SHELL` isn't read. |
| `--bash-timeout-secs N`, `--exec-timeout-secs N` | Command timeout in seconds. The example config uses `120`. |
| `--max-output-bytes N` | Output capture limit. The example config uses `1048576`. |
| `--allow-remote-read` | Allow HTTPS image and audio reads. Off by default. Can't be combined with `--offline`. |
| `--telemetry PATH` | Opt in to owner-only telemetry, kept apart from sessions, with no raw prompts or tool payloads. |

[Tools and permissions](tools.md) explains why full access isn't a sandbox.

Hard token and cost ceilings also need an enforceable provider output limit.
Routes that omit that bound (including Codex Responses, presets that explicitly
omit `max_output_tokens`, and native Responses compaction) refuse hard-ceiling
admission before dispatch. A catalog output maximum doesn't substitute for a
wire-enforced bound. Without those ceilings the ordinary uncapped route stays
available, and that doesn't clear historical usage uncertainty.

## Provider setup

In the interactive first-run flow, an empty catalog with no explicit model
selection opens **Add an API key** first, then **Sign in with ChatGPT / other
supported OAuth subscriptions**, **Local/self-hosted models** and **Continue
without a provider**. Use `/setup` in an existing TUI session to open that
wizard on demand. Your current model, session and default stay in place until
you switch with `/model`. API-key entry is masked and saved only after review,
to owner-private, recoverable storage. It's never a command-line secret
argument. The subscription choices are ChatGPT (Codex), GitHub Copilot, Grok, Kimi Code, Meta and OpenRouter. See
[first-run behavior and credential
privacy](providers.md#first-run-setup-unreleased). The `octet setup` subcommand
below still configures explicit custom endpoints, and print and RPC modes never
open onboarding.

```text
octet --login codex
octet --logout PROVIDER

octet setup --preset lm-studio --manual-model ID [--yes]
octet setup --endpoint URL [--api-key-env VAR] [--model ID|--manual-model ID] [--offline] [--yes]
```

`--headless` prints the device verification URL/code without opening a browser.
`--login`/`--logout` accept `codex` (`openai-codex`, `openai`), `copilot`
(`github-copilot`), `grok` (`xai-subscription`, `supergrok`), `kimi`
(`kimi-code`), `meta` (`muse`), `openrouter`, and `custom` (`openai-custom`). An
unknown provider is rejected with the full list. Each subscription provider
stores one owner-private credential and never reads another provider's.
`--logout` removes only the named provider's credential and makes no network
call. Grok, Kimi, and Meta use a device code, so `--headless` applies to them;
OpenRouter's browser login prints the URL either way. See
[subscription OAuth logins](providers.md#subscription-oauth-logins).

For Codex, `--login codex` offers browser sign-in (PKCE on `127.0.0.1:1455`, or
the registered fallback port `1457`) and device-code sign-in. `--headless` picks
the device flow and prints its verification URL and code without opening a
browser. SSH, no browser opener, or both callback ports being busy also default
or fall back to device code.

The Copilot integration takes `--login copilot [--headless]` and
`--logout copilot`, also under the alias `github-copilot`. It uses only its
private OAuth store, not environment or editor credentials. Online shared
catalogs can then discover eligible `github-copilot/<id>` models, and offline
adds none. First-run subscription setup also offers this device flow through
`/setup`, but TUI `/login copilot` isn't integrated. Native-host protocol 1
gains no auth command or credential field. [Limits and unrun live
qualification](providers.md#github-copilot-unreleased-candidate).

Setup shows a review and writes nothing until you pass `--yes`.
`--preset lm-studio` allows LM Studio's default endpoint, otherwise pass
`--endpoint URL`. `--offline --manual-model ID` sets up without probing
anything. How it works: [Local and custom
endpoints](providers.md#local-and-custom-endpoints).

| Option | What it does |
| --- | --- |
| `--model ID` | Pick from discovered models. |
| `--manual-model ID` | Supply a model when discovery doesn't work. Conflicts with `--model`. |
| `--api-key-env VAR` | Point at a credential in the environment instead of embedding one. |
| `--no-auth` | Use no authentication. Conflicts with `--api-key-env`. |
| `--provider ID` | Custom registry provider ID. Not a display label or a model transport ID. |
| `--label LABEL` | Provider display label. |
| `--replace` | Allow replacing an existing provider entry. Doesn't skip confirmation or the stale-snapshot check. |
| `--yes` | Commit the reviewed change. Rejected if the registry changed since the snapshot. |
| `--cancel` | Cancel without writing. Conflicts with `--yes`. It isn't an offline switch. |

## Sessions and diagnostics

```text
octet --continue
octet --resume [SESSION_ID]
octet --fork [ID|PATH]
octet --session-dir PATH

octet sessions list [--query TEXT]
octet sessions inspect ID
octet sessions rename ID "NAME"
octet sessions tag ID TAG...
octet sessions export ID [--format json|html] [--output PATH] [--force] [--include-secrets]
octet sessions delete ID
octet sessions repair ID
octet doctor
```

`--continue` picks the latest session in this workspace. A bare `--resume` or
`--fork` opens a picker. `--continue`, `--resume` and `--fork` can't be
combined. Fork creates a new session before startup.

`list` and `inspect` are read-only. `delete` moves to a recoverable trash.
`repair` backs up first, then removes only a torn final append. `export` redacts
by default, refuses an existing destination without `--force` and warns on
`--include-secrets`. Both formats always leave out private extension metadata,
so opting out of credential scrubbing doesn't widen that boundary. `doctor` runs
read-mostly checks without starting an agent or an extension. See
[Sessions](sessions.md).

## Local evaluation

```text
octet eval run SUITE [--artifact-dir DIR] [--baseline REPORT.json]
octet eval run SUITE --model-profile /absolute/private-model.json
```

The default is a harness-owned scripted fixture, **not** a model benchmark.
`--model-profile` explicitly selects an independently running local model
server. The profile is an owner-private regular JSON file (at most 16 KiB, no
symlinks or hardlinks) shaped like this:

```json
{
  "schema": "octet-eval-model-1",
  "base_url": "http://127.0.0.1:8000/v1/",
  "model": "operator-selected-model",
  "api_key": "",
  "context_window": 32768,
  "max_output_tokens": 1024,
  "pricing": {"input": 0, "output": 0, "cache_read": 0, "cache_write_5m": 0}
}
```

The endpoint must be literal-loopback HTTP with an explicit non-default port and
a `/v1/` path: no DNS names, remote destinations, redirects, query strings,
custom headers or ambient credentials. `api_key` is required, and empty means
none. Pricing is optional, but when present all four integer rates are required,
in microdollars per million tokens, and explicit zeros declare a free server.
Missing pricing stays **unknown**, not free. Local routing doesn't prove the
server itself avoids downstream paid inference. Model mode rejects scripted
fixture replies in the suite.

<details>
<summary>Limits and reports</summary>

Each case uses a new private HOME, workspace and session, a cleared environment,
no tools or context files, one model turn, and the literal prompt on stdin. The
profile's token limit and any known-price case cost ceiling constrain admission.
Unknown pricing with a cost ceiling refuses before inference, and there's no
aggregate run-wide cost ceiling.

`--case-timeout-ms N` defaults to 60000 (range 1–120000), and
`--max-output-bytes N` defaults to 262144 per stdout or stderr stream (range
1–1048576). Exceeding either bound terminates and reaps the case. A suite is
bounded to 1 MiB, 64 cases and 32 KiB per prompt. Private reports record the
backend, selected model, pass, latency, token and cost measurements, and
baseline deltas. Failed or interrupted calls keep their available durable
accounting, and missing usage or pricing is uncertain, never a made-up exact
zero. Failed cases are observations in the report, not necessarily a nonzero
harness exit.

</details>

## Instructions and resources

| Option | What it does |
| --- | --- |
| `--system-prompt [TEXT]` | Replace the whole composed instruction set. No argument means empty text. AGENTS, context and skills are ignored. |
| `--prompt NAME` | Use a named startup or print prompt. |
| `--debug-prompt` | Show the exact final expansion and template hash before sending. It can reveal sensitive included content. |
| `--prompt-template FILE-OR-DIR` | Explicit prompt source. Repeatable, applied in order. |
| `--theme-dir FILE-OR-DIR` | Extra theme directory or TOML file. Repeated paths use normal resource precedence. [Themes](themes.md). |
| `--skill-dir PATH` | Explicit skill root. |
| `--extension-dir PATH` | Explicit extension source. |
| `--enable-extension NAME` | Enable and save to user config, like Enable in `/extensions`. Not trust. |
| `--trust-extension NAME` | Trust the exact selected source for one run. Not activation. |

[Instructions](instructions.md) and [resource discovery](resources.md) cover
precedence, file limits, trust and reload.

## Packages

Reviewed local archives:

```text
octet extension install --path ARCHIVE
octet extension update --path ARCHIVE
octet extension list
```

The six executable bundles are pinned exactly to the
running host. This checkout's source manifests require `=0.9.0`, and the
published 0.8.0 bundles require `=0.8.0`. The [0.9.0
candidate notes](releases/v0.9.0.md) record planned distribution and remaining
qualification, not publication. The 0.9.0 release assets are unavailable; use
reviewed source extensions or local archives until approved publication.
Catalog installs need verified published assets that match the running host version:

```text
octet extension install NAME
octet extension update NAME
octet extension remove NAME
```

The executable catalog is `octet-codemode`, `octet-computer-use`, `octet-mcp`,
`octet-pi-compat`, `octet-subagents` and `octet-web-search`. Checksummed bundles install atomically
under `~/.octet/extensions/<id>`, and a local update must match the managed
package ID. Nothing runs at install: no hook, dependency setup, activation,
trust or launch. Packaged skills must be loaded explicitly. See
[Extensions](extensions.md).

`--experimental-streamable-http-mcp` is a one-shot opt-in, by the process owner,
to remote MCP that's otherwise blocked. Local stdio MCP doesn't need it. Read
the [MCP gate and
defects](../extensions/octet-mcp/README.md#experimental-streamable-http-gate)
first. It isn't stable.

<a id="pi-interoperability"></a>

## Pi import

```text
octet migrate pi --dry-run [--json] [--pi-home PATH] [--project PATH] [--npm-root PATH]
```

The dry run only reads. It uses no model tokens, runs no package code and
changes no files. `--json` gives versioned machine output, and `--npm-root` adds
an explicit legacy `node_modules` search root. `octet migrate import pi` and
`octet migrate restore` are separate, explicit steps that never copy credentials
or modify Pi sources. Inventory classifications aren't runtime compatibility
claims. The optional [Pi extension adapter](pi-compatibility.md) is a separate
adapter with its own acceptance ledger and explicit
[`--from-pi` setup](pi-compatibility.md#bring-your-pi-extensions). The import and restore bounds are in [Pi
import and restore](pi-migration.md), and [native provider
support](providers.md) is independent of Pi extensions.

<a id="updates-and-legacy-inputs"></a>

## Updates and old flags

`/changelog` opens this binary's **current-version** release notes in the
interactive TUI, including without a configured model. The read-only report uses
rich Markdown, starts at the first row, and supports Up and Down, PageUp and
PageDown, Home and End. Escape or Left closes it. It's available during active
work without interrupting the run or adding notes to the conversation.

<details>
<summary>Where the changelog hint appears</summary>

The muted `/changelog · what's new` hint sits directly below the splash version,
with a shorter fallback in narrow terminals. When startup finds a newer stable
release, an accent update hint follows it, and `octet update` uses rich Markdown
inline-code styling rather than visible backticks. A late result appears once as
a UI-only notice with the same rich action, instead of repainting historical
splash rows. Release notes are compiled into the binary, so no network fetch,
workspace file or model request is used. Plain and print modes reject the
command with guidance to open the interactive TUI. RPC returns its existing
error response for prompt, steer or follow-up invocations. None of these paths
sends release-note requests to a provider or changes an API version.

</details>

`/update` checks for a newer release and points you to `octet update`. That's
the command, not a verified octet channel, and it isn't a path for source
builds. See [availability](installation.md#binary-availability) and [historical
Ygg update behavior](reference/historical-installation.md#updating).

`--safe` is a hidden alias for `--safe-mode`, and `--yolo` is rejected.
`--reasoning-mode pro` only loads old state. Built-in and file theme selection
are in [Themes](themes.md). See [compatibility
inputs](configuration.md#compatibility-inputs).
