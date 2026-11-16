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
