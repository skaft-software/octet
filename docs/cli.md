# CLI reference

[Documentation](README.md) · [Getting started](getting-started.md) · [Slash commands](commands.md)

```sh
octet --safe-mode --model claude-sonnet-4-6
octet -p "Explain the code" --tools read,search
```

This is the documented source surface, not generated help or release
qualification. Uppercase metavariables are values you supply; square brackets
mark optional arguments. Use `octet --help`, `octet sessions --help`,
and `octet migrate pi --help` for authoritative parser details.
The frozen docs are supplemented by a source inventory of static `octet` and
`octet setup` declarations, not a captured `--help` dump. Unlisted nested-command
choices, generated extension flags, and defaults are not inferred here.

## Frontend, model, and workspace

| Form | Contract |
| --- | --- |
| `--print` / `-p`, followed by prompt text | Final response on stdout; does not itself remove tool authority. |
| `--mode rpc` | Pi-compatible JSONL automation frontend; conflicts with `--print`. Separate from native-host protocol 1 and extension API 0.4; [interface limits](terminal.md#choose-a-frontend). |
| `--plain` | Chronological frontend without cursor control. |
| `--color VALUE` | Terminal color selection; documented example `auto`; capability fallbacks still apply. |
| `--mouse auto\|terminal\|off\|app` | Default `auto`; only `app` captures mouse and selects the semantic viewport from startup. |
| `--show-reasoning` | Show reasoning rather than the default collapsed presentation. |
| `--show-images` | Opt in to bounded inline **tool-result display** on compatible interactive terminals; off by default. Not upload permission or input-attachment consent. [Display behavior](terminal.md#tool-evidence-and-worker-activity). |
| `--theme NAME` | Choose built-in `auto`, `light`, `dark`, or a discovered TOML theme by file stem. [Theme discovery](themes.md). |
| `--model ID` | Select model; explicitly overrides a resumed selection. |
| `--reasoning LEVEL` / `--reasoning budget=N` | Model-capability-gated effort or compatible token budget. [Exact levels](providers.md#reasoning). |
| `--cache-retention VALUE` | Provider cache-retention selection; documented example `short`. |
| `--max-turns N` | Bound model turns. |
| `--workspace PATH` | Workspace root for relative tool paths and default bash cwd. |
| `--workspace-trusted` / `--trust-workspace` | Admit project config/instructions/resources; cannot relax global safety floors or grant executable trust. |
| `--no-context-files` | Do not compose context files. |
| `--offline` | Skip optional discovery and the background models.dev metadata refresh, and disable remote media reads; inference can still use the network. |
| `--strict-config` | Treat unknown configuration keys as errors; the default is a warning. Equivalent setting: `strict_config = true`. |

[Terminal behavior](terminal.md), [provider setup](providers.md), and
[configuration values](configuration.md#settings) are separate guides.

RPC assistant messages use the completed response's settled cost, not a fresh
calculation from the current catalog. Their `usage.cost` is `null` when pricing
is unknown; a known zero is distinct. Known total-dollar projections include
sub-microdollar remainder, while exact integer cost remains in the session
ledger. Aggregate scalar costs are known subtotals whenever usage is uncertain
or an operation is unpriced.

## Tools and limits

| Form | Contract |
| --- | --- |
| `--tools NAMES`, `--exclude-tools NAMES` | Final comma-separated allowlist/exclusions; e.g. `read,search`. Model schemas match the executable registry. |
| `--powershell` | Additive opt-in for the Windows `powershell` tool; never replaces `bash`, conflicts with an exclusive `--tools`/`--no-tools` list, and reports itself inert on hosts without PowerShell. |
| `--models PATTERNS` | Ordered, comma-separated model scope for selection and Ctrl+P cycling: `provider/*`, a literal `provider/model`, or a bare-id glob, each with an optional real `:level` suffix. The first requested match is the default for a new session; a miss warns without discarding the rest. `/scoped-models` persists the same ordered patterns. |
| `--no-tools` | Disable tools; conflicts with `--tools`. |
| `--no-edit` | Disable edit and write. |
| `--no-write` | Disable complete-file write. |
| `--no-process`, `--no-shell` | Equivalent no-command authority gates. |
| `--allow-shell` | Enable the shell capability, not a bypass of independent process/effect gates. |
| `--effect-policy controlled_bash_approval\|controlled\|unsafe_host` | Select [effect admission](tools.md#authority-profiles); default `unsafe_host`. |
| `--safe-mode` | Every bash call and workspace mutation requires approval; external paths forced off, executable extensions stopped. Conflicts with `--effect-policy`. |
| `--shell-path PATH` | Explicit Bash-compatible shell; no `$SHELL` lookup. |
| `--bash-timeout-secs N` / `--exec-timeout-secs N` | Command timeout in seconds; supplied config example `120`. |
| `--max-output-bytes N` | Output capture bound; supplied config example `1048576`. |
| `--allow-remote-read` | Opt-in HTTPS image/audio reads; default-off. Conflicts with `--offline`; offline configuration also disables remote reads. |
| `--telemetry PATH` | Owner-only opt-in telemetry, separate from sessions; no raw prompts/tool payloads. |

[Tools and permissions](tools.md) explains why full access is not a sandbox.
Hard token/cost ceilings also require an enforceable provider output limit.
Routes that omit that bound, including Codex Responses, presets explicitly
omitting `max_output_tokens`, and native Responses compaction, refuse hard-ceiling
admission before dispatch. A catalog output maximum is not a substitute for a
wire-enforced bound. Without those ceilings, the ordinary uncapped route remains
available; this does not clear historical usage uncertainty.

## Provider setup

In the interactive first-run flow, an empty catalog with no explicit
model selection opens **Add an API key** first, then **Sign in with ChatGPT /
other supported OAuth subscriptions**, **Local/self-hosted models**, and
**Continue without a provider**. Use `/setup` in an existing TUI session to
open that wizard on demand; the current model/session and default stay in place
until the user explicitly switches with `/model`. API-key entry is masked and
saved only after review to owner-private, recoverable storage; it is not a
command-line secret argument. Subscription choices are ChatGPT (Codex), GitHub
Copilot, Grok, Kimi Code, Meta, and OpenRouter. See
[first-run behavior and credential privacy](providers.md#first-run-setup-unreleased).
The `octet setup` subcommand below still configures explicit custom endpoints;
print/RPC modes never open onboarding.

```text
octet --login codex
octet --logout PROVIDER
# --headless is the provider-auth option; see generated help for its interaction.

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

