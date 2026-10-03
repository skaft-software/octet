# Configuration

[Documentation](README.md) · [CLI](cli.md) · [Providers](providers.md)

Put your own choices in `~/.octet/config.toml`:

```toml
model = "claude-sonnet-4-6"
reasoning = "high"
effect_policy = "controlled_bash_approval"
allow_external_paths = false
```

These are examples, not defaults. Provider credentials don't go in this file:
see [provider setup](providers.md).

## Precedence

Layers, from least to most explicit:

1. Built-in defaults.
2. `~/.octet/config.toml`.
3. A trusted project's `.octet/config.toml`, only with `--workspace-trusted`.
4. Environment variables.
5. CLI flags.
6. The resumed session's model and reasoning. An explicit CLI value still wins.

A trusted project can tighten your authority limits, never relax them. If octet
can't resolve an absolute home directory, it disables global config and
resources and says so. It never substitutes the current directory. System-prompt
precedence follows the same order, and an explicit empty CLI value overrides
every lower layer. `cache_warming` and `show_cache_miss_notices` are user-only:
trusted project config cannot override them. Cache-warming mode is never
restored from session history. `/cache-warming MODE` persists the user choice.

## Settings

Defaults are marked. Other values are examples.

| Setting | Default or example | What it does |
| --- | --- | --- |
| `model` | `claude-sonnet-4-6` | Model ID. The legacy `custom/Qwen3 Coder Next` form still works. Prefer [provider-qualified custom IDs](providers.md#custom-registry) for new entries. |
| `reasoning` | `"high"` | A reasoning choice the model supports. `"off"` is an explicit preference. When it's unset, octet uses [model-aware defaults](providers.md#defaults-unreleased) after restoring the session. [Levels and budgets](providers.md#reasoning). |
| `system_prompt` | `"You are a careful and concise reviewer."` | Replaces all composed system instructions, even with `""`. AGENTS, context and skill instructions are ignored while it's set. |
| `cache_retention` | `"short"` | Provider prompt-cache retention. |
| `cache_warming` | default `"streaming"` | `off`, `streaming`, or `idle`. Additional billable cache refreshes; user level only, never project/session policy. [Scheduling and limits](cache-warming.md). |
| `show_cache_miss_notices` | default `false` | User-level opt-in notices for material cache misses and successful refreshes. Accounting and `/session` diagnostics are unconditional. |
| `theme` | `"auto"` | `auto`, `light` or `dark`, or the file stem of a discovered TOML theme (such as `"mine"`). Auto follows the terminal background. Light and dark override detection. [Themes](themes.md). |
| `color` | `"auto"` | Terminal color, with capability fallbacks. |
| `mouse` | default `"auto"` | `auto`, `terminal` and `off` keep native selection and history. `app` selects the captured viewport. |
| `plain` | `false` | Chronological frontend. |
| `show_images` | default `false` | `true` shows tool-result images inline on compatible terminals. Display only: not upload or attachment consent. Same as `--show-images`. [Limits](terminal.md#tool-evidence-and-worker-activity). |
| `models` | | Optional user-level ordered model scope, written by `/scoped-models` as one comma-separated pattern string (such as `"openai/*:high,custom/alpha-model"`). It only affects Ctrl+P cycling in the interactive UI. Headless modes ignore it, `--models` wins if both are set, and a trusted project layer can't override it. |
| `effect_policy` | default `"unsafe_host"` | Or `"controlled"` or `"controlled_bash_approval"`. [Permissions](tools.md#authority-profiles). |
| `allow_external_paths` | default `true` in full-access CLI launches | `false` keeps the built-in file tools in the workspace. Safe mode forces `false`. It doesn't contain shell commands or extension processes. |
| `allow_edit`, `allow_write` | `true` for both | Separate file-change switches. `--no-edit` removes both tools. |
| `allow_process`, `allow_shell` | `true` for both | Separate process and shell gates. Enabling one doesn't override an effect denial. |
| `allow_remote_read` | default `false` | Allow HTTPS image and audio reads. `--offline` always disables it. |
| `shell_path` | optional | Explicit Bash-compatible shell. [Selection order](tools.md#shell-selection). |
| `bash_timeout_secs` | `120` | Command timeout in seconds. |
| `max_output_bytes` | `1048576` | Command output capture limit. |
| `context_files` | `true` | Include instruction and context files. Project files still need trust. |
| `offline` | `false` | `true` skips optional model discovery and remote reads, not inference. |
| `strict_config` | default: warn | `true` makes unknown keys errors, like `--strict-config`. |
| `reload` | default `true` | The interactive prompt quietly starts the live-reload supervisor and applies reloads only at the idle prompt. `/reload --dry-run` shows watch counts, timing and the host re-exec policy, and incomplete watch coverage still warns. `false` turns sampling off for good. User level only: a trusted project layer can't turn it on. |
| `reload_poll_ms` | default `1000` | Interval between filesystem samples, clamped to `50..=300000`. Sampling covers the skill, prompt, theme, context and extension roots in use, plus the resolved executable. |
| `reload_debounce_ms` | default `200` | Save-burst debounce, clamped to a maximum of `2000` so a burst always flushes. |
| `reload_max_files` | default `512` | Metadata inspections per poll, clamped to `1..=4096`. Directory listing has its own allowance of the same size, plus at most one overflow entry, and entries are bounded before collection and sorting. A partly scanned layer is reported as capped, never as a change or a removal. |
| `session_dir` | | Session storage root. Same as `--session-dir PATH`. [Sessions](sessions.md). |
| `max_turns` | | Limit model turns. Same as `--max-turns N`. |
| `max_cost_microdollars` | `500000` | Optional session cost limit, in integer microdollars. |
| `cost_warning_microdollars` | `50000` | Optional cost warning, in integer microdollars. |
| `telemetry` | `"./artifacts/octet-telemetry.jsonl"` | Optional JSONL output path. Off unless set. |
| `[compaction]` | | `mode = "local"`, `threshold_fraction = 1.0`, optional `max_active_tokens` (zero or unset uses the model limit), `keep_recent_tokens = 20000`, optional `compact_model = "provider/model"`. [Details](context.md#settings). |
| `enabled_extensions` | default `[]` | Installed executable extensions stay disabled until you enable them. Full access doesn't change activation. |
| `trusted_extensions` | default `[]` | Persistent **host authority grants** (existing configs keep their meaning). A bare name grants only the global source. `NAME@/absolute/path/extension.toml` grants that exact manifest. Full access implicitly authorizes enabled extensions without writing a grant. Safe mode starts only enabled, explicitly granted sources (or an explicit `--extension-dir`). Extension code runs with your OS permissions, outside the tool-effect broker. [Resource rules](resources.md#locations-and-precedence). |

`--theme-dir` adds a theme directory or TOML file to bounded discovery. Named
files from global, trusted project or explicit roots can be loaded at startup
and chosen with `/theme`. See [Themes](themes.md).

<details>
<summary>How the reload caps report</summary>

Cap reports show **at least** the known skipped paths, not an exact total,
because unread directory contents are unknown. Failed directory entries also use
up the listing allowance. Fully scanned directories keep a deterministic order.
A capped layer neither replaces its baseline nor infers changes from an
arbitrary filesystem-order prefix. The first complete scan sets that layer's
baseline. Executable sampling is separate from these resource-tree limits.

</details>

## Environment variables

| Variable | What it sets |
| --- | --- |
| `OCTET_MODEL`, `OCTET_REASONING` | Model and effort. |
| `OCTET_EFFECT_POLICY` | Effect profile. |
| `OCTET_SYSTEM_PROMPT` | System-instruction replacement ([precedence](#precedence)). |
| `OCTET_CACHE_RETENTION` | Cache retention. |
| `OCTET_CACHE_WARMING` | `off`, `streaming`, or `idle`; overrides the user config, below `--cache-warming`. |
| `OCTET_COLOR`, `OCTET_MOUSE`, `OCTET_THEME`, `OCTET_COLOR_SCHEME` | Terminal presentation. `OCTET_THEME` takes `auto`, `light`, `dark` or a discovered TOML theme name. `OCTET_COLOR_SCHEME` stays a background-detection override. |
| `OCTET_TERN`, `OCTET_TUI_TERN` | Tern native rendering: `auto`, `on` or `off`. Defaults to `auto`, which negotiates native surfaces only inside a Tern pane. `OCTET_TUI_TERN` is the older spelling and is read only when `OCTET_TERN` is unset. |
| `OCTET_SHOW_IMAGES` | `1` shows tool-result images inline. It isn't a media upload. |
| `OCTET_WORKSPACE`, `OCTET_SESSION_DIR` | Workspace and session roots. |
| `OCTET_MAX_TURNS` | Turn limit. |
| `OCTET_COMPACTION_MODE`, `OCTET_COMPACTION_THRESHOLD_FRACTION`, `OCTET_COMPACTION_MAX_ACTIVE_TOKENS` | Compaction. |
| `OCTET_SHELL_PATH`, `OCTET_BASH_TIMEOUT_SECS`, `OCTET_MAX_OUTPUT_BYTES` | Shell and command limits. |
| `OCTET_OFFLINE` | Skip optional discovery and the background models.dev metadata refresh. Not network isolation. |
| `OCTET_TELEMETRY` | Telemetry path. |
| `OCTET_ALLOW_*` | The capability switches. `OCTET_ALLOW_REMOTE_READ=true` allows remote media reads unless offline. |
| `OCTET_PACKAGE_DIR`, `OCTET_DATA_DIR` | Override the [self-documentation root](instructions.md#self-documentation). |
| `OCTET_TUI_WRITE_LOG` | Raw terminal capture. Sensitive: see below. |

## Diagnostics and telemetry

`--telemetry PATH` writes owner-only `octet.telemetry.v1` JSONL, separate from
your sessions. Raw prompts, tool arguments, results and provider payloads aren't
logged, and prompt identity and tool arguments are hashed. See the [schema and
method](benchmarks/README.md). `OCTET_TUI_WRITE_LOG=/path/to/ansi.log` captures
the interactive screen's raw ANSI stream, into a unique
`tui-<timestamp>-<pid>.log` if the path is an existing directory. It's off by
default, and captured prompts and tool output make those logs sensitive even
though telemetry is secret-safe.

<details>
<summary>What telemetry records</summary>

Run boundaries, model latency and time to first token, input, cache and output
usage (counted separately), retries, tool timings and repetition signals,
compaction outcomes, terminal status, and secret-safe effect admission. An
admission record holds the effect, a stable denial code, the effective policy
values and each config source layer. Shell identity is only a non-correlating
resolution branch, never a path or digest.

</details>

## Compatibility inputs

Old names keep working so existing setups don't break. None of them imply Ygg
command aliases, old-root discovery or an automatic first-party migration.

<details>
<summary>The old inputs and what they do now</summary>

- `OCTET_EXEC_TIMEOUT_SECS` still works as a fallback for the previous timeout
  name.
- `[compaction] enabled = true` and `OCTET_AUTO_COMPACT=true` select `local`.
- `reasoning_mode = "pro"`, `OCTET_REASONING_MODE=pro` and
  `--reasoning-mode pro` only load legacy config and sessions. With complete
  current Ultra/V2 support they migrate to `reasoning = "ultra"`. Otherwise
  octet drops the obsolete mode, keeps any separately selected supported effort
  and warns. New config uses `reasoning` alone.
- `--safe` is a hidden alias of `--safe-mode`. `--yolo`, and its config and
  environment forms, are rejected.

</details>
