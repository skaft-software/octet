# CLI reference

[Documentation](README.md) · [Getting started](getting-started.md) · [Slash commands](commands.md)

```sh
octet --safe-mode --model claude-sonnet-4-6
octet -p "Explain the code" --tools read,search
```

This is the documented source surface, not generated help or release
qualification. Uppercase metavariables are values you supply; square brackets
mark optional arguments. Use `octet --help`, `octet sessions --help`,
`octet migrate pi --help`, and `octet pi --help` for authoritative parser details.
The frozen docs are supplemented by a source inventory of static `octet` and
`octet setup` declarations, not a captured `--help` dump. Unlisted nested-command
choices, generated extension flags, and defaults are not inferred here.

## Frontend, model, and workspace

| Form | Contract |
| --- | --- |
| `--print` / `-p`, followed by prompt text | Final response on stdout; does not itself remove tool authority. |
| `--mode rpc` | Pi-compatible JSONL automation frontend; conflicts with `--print`. Separate from native-host protocol 1 and extension API 0.3; [interface limits](terminal.md#choose-a-frontend). |
| `--plain` | Chronological frontend without cursor control. |
| `--color VALUE` | Terminal color selection; documented example `auto`; capability fallbacks still apply. |
| `--mouse auto\|terminal\|off\|app` | Default `auto`; only `app` captures mouse and selects the semantic viewport from startup. |
| `--show-reasoning` | Show reasoning rather than the default collapsed presentation. |
| `--show-images` | Opt in to bounded inline **tool-result display** on compatible interactive terminals; off by default. Not upload permission or input-attachment consent. [Display behavior](terminal.md#tool-evidence-and-worker-activity). |
| `--model ID` | Select model; explicitly overrides a resumed selection. |
| `--reasoning LEVEL` / `--reasoning budget=N` | Model-capability-gated effort or compatible token budget. [Exact levels](providers.md#reasoning). |
| `--cache-retention VALUE` | Provider cache-retention selection; documented example `short`. |
| `--max-turns N` | Bound model turns. |
| `--workspace PATH` | Workspace root for relative tool paths and default bash cwd. |
| `--workspace-trusted` / `--trust-workspace` | Admit project config/instructions/resources; cannot relax global safety floors or grant executable trust. |
| `--no-context-files` | Do not compose context files. |
| `--offline` | Skip optional discovery and disable remote media reads; inference can still use the network. |
| `--strict-config` | Treat unknown configuration keys as errors; the default is a warning. Equivalent setting: `strict_config = true`. |

[Terminal behavior](terminal.md), [provider setup](providers.md), and
[configuration values](configuration.md#settings) are separate guides.

## Tools and limits

| Form | Contract |
| --- | --- |
| `--tools NAMES`, `--exclude-tools NAMES` | Final comma-separated allowlist/exclusions; e.g. `read,search`. Model schemas match the executable registry. |
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

## Provider setup

```text
octet --login codex
octet --logout PROVIDER
# --headless is the provider-auth option; see generated help for its interaction.

octet setup --preset lm-studio --manual-model ID [--yes]
octet setup --endpoint URL [--api-key-env VAR] [--model ID|--manual-model ID] [--offline] [--yes]
```

`--headless` is the documented provider-auth option; consult generated help for
its exact login interaction. Copilot is not a CLI login/configuration provider.

Setup reviews without writing by default. `--yes` commits only the reviewed
transaction; `--cancel` leaves the registry unchanged. An explicit `--preset
lm-studio` permits its default endpoint; otherwise choose `--endpoint URL`.
`--api-key-env VAR` references a credential rather than embedding one.
`--model ID` selects discovered inventory; `--manual-model ID` supplies a model
when discovery is unsuitable. Use `--offline --manual-model ID` for no-probe
recovery.

Additional setup options apply to either recipe:

| Form | Contract |
| --- | --- |
| `--provider ID` | Choose the custom registry provider ID, not a display label or model transport ID. |
| `--label LABEL` | Set the provider display label. |
| `--no-auth` | Explicitly select no authentication instead of `--api-key-env VAR`. |
| `--replace` | Permit replacement of an existing provider entry; does not bypass confirmation or stale-snapshot checks. |
| `--cancel` | Cancel without writing the registry; not an offline/no-probe switch. |

The pairs `--api-key-env` / `--no-auth`, `--yes` / `--cancel`, and `--model` /
`--manual-model` conflict. Preview is not a write: `--yes` confirms the prepared
transaction, whose compare-and-swap rejects a registry changed since its snapshot.
Review/cancel/stale-snapshot failures leave the registry unchanged.
[Privacy, discovery bounds, and transaction failures](providers.md#local-and-custom-endpoints).

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
octet sessions export ID [--output PATH] [--force] [--include-secrets]
octet sessions delete ID
octet sessions repair ID
octet doctor
```

`--continue` selects the latest current-workspace session; bare `--resume` or
`--fork` opens a picker. `--continue` and `--resume` conflict; `--fork` conflicts
with both. Fork creates a new session before startup. Listing and
inspection are read-only. Delete moves to recoverable trash; repair backs up
before removing only a torn final append. Export redacts by default, refuses an
existing destination without `--force`, and warns for `--include-secrets`.
`doctor` performs read-mostly prerequisite/provider/model checks without an Agent
or executable-extension startup. [Sessions](sessions.md).

## Instructions and resources

| Form | Contract |
| --- | --- |
| `--system-prompt [TEXT]` | Entire composed-instruction override; no argument means explicit empty text. AGENTS/context/skills are ignored. |
| `--prompt NAME` | Select a named startup/print prompt. |
| `--debug-prompt` | Show exact final expansion and template hash before provider submission; can expose sensitive included content. |
| `--prompt-template FILE-OR-DIR` | Explicit prompt source, repeatable in order. |
| `--skill-dir PATH` | Explicit skill root. |
| `--extension-dir PATH` | Explicit executable-extension source. |
| `--enable-extension NAME` | One-invocation activation; not trust. |
| `--trust-extension NAME` | One-invocation trust of the selected exact source; not activation. |

[Instructions/prompts/skills](instructions.md) and [resource discovery](resources.md)
cover precedence, file bounds, trust, and reload.

## Packages and Serve

For reviewed local archives:

```text
octet extension install --path ARCHIVE
octet extension update --path ARCHIVE
octet extension list
```

The four official executable bundles and the separate Serve application must
match octet 0.7.5. Availability, signed assets, and public-install verification
are recorded on the [version-pinned GitHub release](https://github.com/skaft-software/octet/releases/tag/v0.7.5).
Catalog forms select the package matching the running host version:

```text
octet extension install NAME
octet extension update NAME
octet extension remove NAME
```

The executable catalog is `octet-browse`, `octet-mcp`, `octet-subagents`, and
`octet-web-search`. Checksummed bundles publish atomically under
`~/.octet/extensions/<id>`; local updates must match the managed package ID.
No install hook, dependency provisioning, activation, trust, or process launch
occurs. Packaged skills require explicit loading. [Packaging contract](extensions.md).

Serve is a separate version-matched application package. With a reviewed,
compatible package installed, `octet serve` starts its loopback web interface;
`octet serve --no-open --port 0` avoids opening a browser and lets the OS select a
port. `extension install/update/remove octet-serve` use the published catalog;
local archive forms above also apply. Removal leaves sessions
and other Serve data intact. [Serve setup and limits](experimental/octet-serve/README.md).

`--experimental-streamable-http-mcp` is a conspicuous **one-shot process-owner**
opt-in for otherwise-blocked remote MCP. It is not required for local stdio MCP.
Read the [MCP package's gate and defects](../extensions/octet-mcp/README.md#experimental-streamable-http-gate)
before use; this is not stable transport qualification.

## Pi interoperability

```text
octet migrate pi --dry-run [--json] [--pi-home PATH] [--project PATH] [--npm-root PATH]
octet pi install PATH
octet pi list
```

Inventory reads bounded Pi settings/manifests and local/npm/git package locations,
parses JS/TS/TSX with tree-sitter, and consumes zero model tokens. It executes no
package code, starts no provider/model, changes no files, and does not copy
resources/apply recipes. `--json` is versioned machine output; `--npm-root` adds
only an explicit legacy `node_modules` search root.

Separate explicit `octet migrate import pi` and `octet migrate restore` cover a
bounded portable subset without copying credentials or modifying Pi sources.
`pi install` links reviewed local sources inertly; the pinned bridge remains
disabled and untrusted until activated. Exact import/restore, plan/preflight,
compatibility profile, bounds, and gaps stay in [Pi migration](pi-migration.md).

## Updates and legacy inputs

`/changelog` opens this binary's **current-version** release notes in the
interactive TUI, including without a configured model. The read-only report uses
rich Markdown, starts at the first row, and supports Up/Down, PageUp/PageDown,
Home, and End; Escape or Left closes it. It remains available during active work
without interrupting the run or adding notes to the conversation. The muted
`/changelog · what's new` hint sits directly below the splash version, with a
shorter fallback in narrow terminals. When startup finds a newer stable release,
an accent update hint follows it; `octet update` uses rich Markdown inline-code
styling rather than visible backticks. A late result appears once as a UI-only
notice with the same rich action instead of repainting historical splash rows.

Release notes are compiled into the binary: no network fetch, workspace file, or
model request is used. Plain and print modes reject the command with guidance to
open the interactive TUI; RPC returns its existing error response for prompt,
steer, or follow-up invocations. Serve treats it as an unsupported boundary,
without inferring an answer. None of these paths sends release-note requests to
a provider or changes an API version.

`/update` checks for a newer release and directs you to `octet update` to install;
that is an update command contract, not a verified octet channel. Do not treat it
as a source-build release-promotion path. [Current availability](installation.md#binary-availability)
and [historical Ygg update behavior](reference/historical-installation.md#updating)
are deliberately separate.

`--safe` is hidden compatibility for `--safe-mode`; `--yolo` is rejected.
`--reasoning-mode pro` loads legacy state only. `--theme-dir` and arbitrary
theme names remain compatibility inputs; built-in terminal appearance choices
are documented in [Theme status](themes.md). See [compatibility inputs](configuration.md#compatibility-inputs).
