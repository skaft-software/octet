# Configuration

[Documentation](README.md) · [CLI](cli.md) · [Providers](providers.md)

Put deliberate user choices in `~/.octet/config.toml`, for example:

```toml
model = "claude-sonnet-4-6"
reasoning = "high"
effect_policy = "controlled_bash_approval"
allow_external_paths = false
```

These are example choices, not a claim that this model is configured or that all
values are defaults. Provider credentials belong in the [provider setup](providers.md),
not this file.

## Precedence

The supplied reference orders layers from least to most explicit:

1. Built-in defaults.
2. `~/.octet/config.toml`.
3. Trusted project `.octet/config.toml`, only with `--workspace-trusted`.
4. Environment variables.
5. CLI flags.
6. Resumed-session model/reasoning, unless explicitly overridden by CLI.

A trusted project can tighten user authority floors, not relax them. If an
absolute user home cannot be resolved, global config/resources are disabled
with a diagnostic; octet never substitutes the invocation directory.

System-prompt precedence follows the same order: global configuration, trusted
project configuration, environment, then CLI. An explicit empty CLI value
overrides all lower layers.

## Settings

Only explicitly stated defaults below are defaults. Other numeric/string values
preserve the supplied reference's example configuration, not newly verified
runtime defaults.

| Setting | Meaning and documented value |
| --- | --- |
| `model` | Model ID; examples include `claude-sonnet-4-6` and legacy `custom/Qwen3 Coder Next`. Prefer [provider-qualified custom IDs](providers.md#custom-registry) for new registry entries. |
| `reasoning` | Model-supported choice, example `"high"`; `"off"` is an explicit preference. Unset uses [model-aware defaults](providers.md#defaults-unreleased), after session restoration. [Levels and budgets](providers.md#reasoning). |
| `system_prompt` | Replace all composed system instructions, including with `""`; example `"You are a careful and concise reviewer."`. AGENTS/context/skill instructions are ignored while set. |
| `cache_retention` | Provider prompt-cache retention selection; example `"short"`. |
| `theme` | Built-in `"auto"`, `"light"`, or `"dark"`. Auto adapts to the terminal background; light/dark override detection. |
| `color` | Terminal color selection; example `"auto"`, with terminal-capability fallbacks. |
| `mouse` | Default `"auto"`; `auto`, `terminal`, and `off` preserve native selection/history; `app` selects the captured semantic viewport. |
| `plain` | Chronological frontend; example `false`. |
| `show_images` | Default `false`; `true` opts in to bounded inline tool-result images on compatible interactive terminals. This controls display, not upload or explicit input-attachment consent. Equivalent flag: `--show-images`. [Display limits](terminal.md#tool-evidence-and-worker-activity). |
| `models` | Optional user-level ordered model scope written by `/scoped-models` as one comma-separated pattern string (e.g. `"openai/*:high,custom/alpha-model"`). Interactive Ctrl+P cycling only: headless modes ignore it, `--models` wins when both are present, and a trusted project layer can never override it. |
| `effect_policy` | Default `"unsafe_host"`; alternatives `"controlled"`, `"controlled_bash_approval"`. [Authority profiles](tools.md#authority-profiles). |
| `allow_external_paths` | Default `true` in full-access CLI launches. Set `false` for workspace-local built-in file admission; safe mode forces false. This does not contain shell commands or extension processes. |
| `allow_edit`, `allow_write` | Independent mutation capabilities; example `true` for both. `--no-edit` removes both tools. |
| `allow_process`, `allow_shell` | Independent process/shell gates; example `true` for both. Enabling does not override an effect denial. |
| `allow_remote_read` | Default `false`; opt-in HTTPS image/audio reads, always disabled by `--offline`. |
| `shell_path` | Optional explicit Bash-compatible shell; [selection order](tools.md#shell-selection). |
| `bash_timeout_secs` | Command timeout; example `120` seconds. |
| `max_output_bytes` | Command output capture bound; example `1048576` bytes. |
| `context_files` | Include instruction/context files; example `true`, project inputs still require trust. |
| `offline` | Example `false`; `true` skips optional model discovery and remote reads, not inference. |
| `strict_config` | Default behavior warns about unknown keys; `true` makes them errors, as does `--strict-config`. |
| `reload` | Default `true`: the interactive prompt silently arms the live-reload supervisor and applies reloads only at the idle prompt. `/reload --dry-run` shows watch counts, timing, and host re-exec policy; incomplete watch coverage still warns. `false` disables sampling for good. User level only; a trusted project layer may not arm it. |
| `reload_poll_ms` | Default `1000`; interval between filesystem samples, clamped to `50..=300000`. Sampling covers the skill/prompt/theme/context/extension roots in use plus the resolved executable. |
| `reload_debounce_ms` | Default `200`; save-burst debounce, clamped to `2000` maximum so a burst always flushes. |
| `reload_max_files` | Default `512`; metadata inspections per poll, clamped to `1..=4096`. Directory enumeration shares a separate allowance of the same size, plus at most one overflow entry; entries are bounded before collection/sorting. Partially scanned layers are reported as capped, never as changes or removals. |
| `session_dir` | Session-storage root; equivalent CLI option `--session-dir PATH`. [Storage and recovery](sessions.md). |
| `max_turns` | Bound model turns; equivalent CLI option `--max-turns N`. |
| `max_cost_microdollars` | Optional session cost guardrail; example `500000`, integer microdollars. |
| `cost_warning_microdollars` | Optional cost warning; example `50000`, integer microdollars. |
| `telemetry` | Optional explicit JSONL output path; disabled unless set. Example `"./artifacts/octet-telemetry.jsonl"`. |
| `[compaction]` | `mode = "local"`, `threshold_fraction = 1.0`, optional `max_active_tokens` (zero/unset uses model limit), `keep_recent_tokens = 20000`, optional `compact_model = "provider/model"`. [Exact budgeting and caveats](context.md#settings). |
| `enabled_extensions` | Default `[]`: installed executable extensions stay disabled until explicitly enabled. Full access does not change activation. |
| `trusted_extensions` | Default `[]`: optional persistent source-bound grants. Full access implicitly trusts selected extensions without adding grants; safe mode removes implicit trust and blocks executable startup even with explicit grants. [Resource rules](resources.md#locations-and-precedence). |

Reload cap reports show **at least** the known skipped paths, not an exact total:
unread directory contents are unknown. Failed directory entries also consume the
enumeration allowance. Fully scanned directories retain deterministic ordering;
capped layers neither replace their baseline nor infer changes from an arbitrary
filesystem-order prefix. The first complete scan establishes that layer's
baseline. Executable sampling remains independent of these resource-tree limits.

## Environment variables

| Variable | Corresponding control |
| --- | --- |
| `OCTET_MODEL`, `OCTET_REASONING` | Model and effort. |
| `OCTET_EFFECT_POLICY` | Effect profile. |
| `OCTET_SYSTEM_PROMPT` | System-instruction replacement; see [precedence](#precedence). |
| `OCTET_CACHE_RETENTION` | Cache retention. |
| `OCTET_COLOR`, `OCTET_MOUSE`, `OCTET_THEME`, `OCTET_COLOR_SCHEME` | Terminal presentation; `OCTET_THEME` accepts `auto`, `light`, or `dark`, while `OCTET_COLOR_SCHEME` remains a background-detection override. |
| `OCTET_SHOW_IMAGES` | `1` opts in to inline tool-result display, not media upload. |
| `OCTET_WORKSPACE`, `OCTET_SESSION_DIR` | Workspace and session-storage roots. |
| `OCTET_MAX_TURNS` | Turn bound. |
| `OCTET_COMPACTION_MODE`, `OCTET_COMPACTION_THRESHOLD_FRACTION`, `OCTET_COMPACTION_MAX_ACTIVE_TOKENS` | Compaction mode and thresholds. |
| `OCTET_SHELL_PATH`, `OCTET_BASH_TIMEOUT_SECS`, `OCTET_MAX_OUTPUT_BYTES` | Shell and command limits. |
| `OCTET_OFFLINE` | Skip optional discovery; not network isolation. |
| `OCTET_TELEMETRY` | Opt-in telemetry path. |
| `OCTET_ALLOW_*` | Mirrored capability controls; specifically `OCTET_ALLOW_REMOTE_READ=true` grants remote media reads unless offline. |
| `OCTET_PACKAGE_DIR`, `OCTET_DATA_DIR` | Override the [self-documentation asset root](instructions.md#self-documentation). |
| `OCTET_TUI_WRITE_LOG` | Opt-in sensitive raw terminal capture, below. |

## Diagnostics and telemetry

`--telemetry PATH` writes owner-only `octet.telemetry.v1` JSONL, separately from
durable sessions. It records run boundaries, model latency/TTFT, disjoint
input/cache/output usage, retries, tool timings/repetition signals, compaction
outcomes, terminal status, and secret-safe effect admission. Decisions contain
effect, stable denial code, effective policy values, and each configuration
source layer. Shell identity is only a non-correlating resolution branch, never
a path/digest. Prompt identity and tool arguments are hashed: raw prompts,
arguments, results, and provider payloads are not logged. See the
[telemetry schema and measurement methodology](benchmarks/README.md).

`OCTET_TUI_WRITE_LOG=/path/to/ansi.log` captures the interactive frontend's raw
ANSI stream. An existing directory instead gets a unique
`tui-<timestamp>-<pid>.log`. Disabled by default; captured prompts/tool output
make these logs sensitive even when telemetry is secret-safe.

## Compatibility inputs

- `OCTET_EXEC_TIMEOUT_SECS` remains a fallback for the previous timeout name.
- `[compaction] enabled = true` and `OCTET_AUTO_COMPACT=true` select `local`.
- `reasoning_mode = "pro"`, `OCTET_REASONING_MODE=pro`, and `--reasoning-mode pro` only load legacy config/sessions. They migrate to `reasoning = "ultra"` only with complete current Ultra/V2 support; otherwise octet removes the obsolete mode, retains independently selected supported effort, and warns. New config uses `reasoning` alone.
- `--safe` is a hidden alias of `--safe-mode`; `--yolo` and its config/environment forms are rejected.
- `--theme-dir` and arbitrary theme names remain compatibility inputs and never load filesystem themes. The built-in `theme` choices are documented in [Theme status](themes.md).

Compatibility inputs do not imply Ygg command aliases, old-root discovery, or an
automatic first-party migration.
