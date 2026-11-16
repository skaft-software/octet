# Tools and extension-host dependency map

Baseline: `882c491c` (before the feature split). Ultra's requested imports are
already public, but `octet-agent` has no compilation boundary for them.

## Transitive module dependencies measured before editing

Source inspection follows `crate::` references, module declarations and imports;
this is a module dependency map, not a claim that every function is called.

| Requested surface | Direct implementation | Transitive infrastructure |
| --- | --- | --- |
| `BashTool`, `EditTool`, `ReadTool`, `WriteTool` | `tools::{bash,edit,read,write}` | `tool`, `effect`, `sandbox`, `secure_fs`, `tools::{bash_spill,powershell,shell_environment,durability}` |
| `Tool`, `ToolContext`, output/progress types, cancellation | `tool` | `effect`, `sandbox`, `tools::durability`, `tool_composition`, `tool_inference`, `extension_operations` |
| `SandboxConfig`, `ToolEffect`, `EffectBroker`, `EffectPolicy`, `EffectIntent` | `sandbox`, `effect` | `tool` (confirmation/progress); `secure_fs` for built-in execution |
| Manifest/discovery/activation/hook/source/trust types | `extension_process::{manifest,contributions,host_requests,runtime_config,protocol}` | `extension_api_{policy,v03,v04}`, `extension_policy`, `extension_presentation`, `extension_menu`, `extension_remote_ui` |
| `ExtensionProcess`, host/execution/owner/tool types | `extension_process` | its process, connection, admission, reader, resource, lifecycle, provider and service modules; `artifact`, `extension`, `extension_bulk`, `extension_bulk_host`, `extension_diagnostics`, `extension_operations`, `extension_provider`, `extension_secret`, `tool_composition`, `tool_inference`, `input` |

The closure currently reaches **every top-level implementation module** except
`extension_runtime`, `goal_driver` and `goal_store` (these are nevertheless
unconditionally compiled). Coupling into orchestration comes from:

- `tool`: session progress variants/acknowledgements, skill snapshots and causal
  session-driver routing.
- `tools::durability` and `tools::deferred`: optional `SessionWriter` backends.
- `extension`: event observers, cache advice, provider-context preparation,
  persistence hooks and session-operation hooks.
- `extension_process`: delegation service, session-leaf consumers/snapshots,
  context hooks, transcript-entry helpers and nested inference timestamps.
- Those edges reach `agent` (including `provider_context`), `session` and its
  writer/replay/context/journal, `session_leaf`, `compaction`, `delegation`,
  `cache`, `cache_warmer`, `events`, `context`, `telemetry`, `skills` and input
  reconstruction. `octet-ai` remains necessary for canonical tool/media/model
  and provider protocol types; this task does not split that package.

Pi compatibility reference: the owner's Pi source exports standalone tool
factories in `packages/coding-agent/src/core/tools/index.ts:1–80,118–139`.
Cargo feature boundaries are Octet packaging, not a new Pi wire or behavior.

## Acceptance

The reduced build must preserve the requested imports and executable tools and
process host while omitting concrete agent-loop and session persistence modules.
Default features preserve the existing API and behavior. Compile-time and
non-test compiled-source measurements are recorded with the worker's checks;
source counts must distinguish shared wire vocabulary from orchestration.

## Implemented compilation boundary

`agent` is default-on; `agent -> tools -> extensions`. The reduced selection is
`--no-default-features --features tools,extensions`. `extensions` alone leaves
out built-in implementations; no features exports no infrastructure. Cargo
feature unification can re-enable `agent`, so the independent acceptance crate
checks resolved compiler features rather than trusting manifest spelling.

Module-boundary gates omit `agent` (including its provider-context driver),
`session`, `session_writer`, `session_leaf`, `compaction`, `delegation`, `cache`,
`cache_warmer`, `context`, `telemetry`, `goal_driver`, `goal_store`, `skills` and
`extension_runtime`. Agent-specific extension session-leaf/provider-context
modules and durable deferred/summary tooling are also omitted. Shared
`extension_message` and `session_types` contain only message validation,
identifiers and error vocabulary; original default-feature paths re-export the
same types. `AgentModelSelection` belongs to host request vocabulary. Encoded
provider pipeline hooks remain with the extension host, separate from canonical
session-context preparation. In-memory invocation memos remain usable without
the `SessionWriter` backend. No second implementation or crate is introduced.

`examples/tools-extensions-only` imports exactly the requested downstream list.
Its dependency script checks resolved features, compiler dep-info inputs and
nineteen separate compile-fail import probes, plus three offline runtime tests.
The process peer checks that excluded agent/session transport services are not
offered. Default tests keep their existing assertions and ignored-test policy.

## Local compilation measurements

Linux, Rust/Cargo 1.97.1, two build jobs, non-incremental builds on a shared host.
Dependencies were warmed first; touching only `src/lib.rs` forced each package
recheck. Three alternating `cargo check -p octet-agent` and
`cargo check -p octet-agent --no-default-features --features tools,extensions`
runs measured root-package checking only, not clean dependency builds:

| Metric | Default | Reduced | Reduction |
| --- | ---: | ---: | ---: |
| Wall seconds, three runs | 40.86 / 32.19 / 32.31 | 11.09 / 10.07 / 11.94 | shared-host observations |
| Median wall seconds | 32.31 | 11.09 | 21.22 s (65.7%) |
| Expanded non-test implementation lines | 276,507 | 170,549 | 105,958 (38.3%) |

The line metric counts compiler-expanded, cfg-resolved nonblank lines excluding
comments and rustdoc attributes, **including generated derives**, for this crate
only. It is not a handwritten-source or downstream binary-size measurement.
It was captured with `RUSTC_BOOTSTRAP=1 cargo rustc -p octet-agent --lib --
-Zunpretty=expanded`, then the same command with `--no-default-features --features
tools,extensions` before `--`. CI uses only stable checks; the internal compiler
view is a local measurement aid. No live provider or downstream application
qualification is implied by these numbers.
