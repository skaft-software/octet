# Pi 1.0.2 extension API ledger

Target: Pi 1.0.2 (`200387122ca450d6387f033949423114a270b96c`),
`packages/coding-agent/src/core/extensions/types.ts` and `docs/extensions.md`.
Octet replicates that public API under its extension protocol through the
`octet-pi-compat` Node adapter. Third-party extensions, private Pi modules, the
Pi CLI and the child SDK are not targets.

Status:
**done** (the public behavior passed at least one real Rust-host acceptance
path: real `App`/agent, the real extension process running the adapter with a
Pi fixture, real tools, policy and persistence; only the model provider is
scripted), **implemented** (code and adapter tests exist; no real-host
acceptance yet), **partial** (narrower than Pi), **wrong** (different
semantics), **missing**. Adapter tests against the synthetic host never make
a row done.

Per-example routes, the latest native results, deferrals and merge notes are
in [pi-compat-release-status.md](pi-compat-release-status.md).

| # | Area | Pi members | Status | Code to write |
|---|------|-----------|--------|---------------|
| 1 | Tool call event | `tool_call`: mutate `event.input`, `{block, reason, terminate}` | **done** | Hooks run before admission; the broker authorizes and the tool runs the final arguments. Like Pi, mutated arguments are not re-validated against the schema. `block`, `terminate` and sibling handling pass native `pi_tool_hooks_tests`. |
| 2 | Tool result event | `tool_result`: return `{content, details, structuredContent, isError, usage}` | **done** | Chained per Pi; content without `structuredContent` drops it; policy denials stay denials. `usage` is billed and kept out of provider context; image replacement keeps order and is persisted once (native `pi_tool_hooks_tests`). |
| 3 | Custom messages | `sendMessage(msg, {deliverAs, triggerTurn})`, `display`, `details`, `sendUserMessage(text, {deliverAs})`, `before_agent_start` `message` | **partial** | Real-host interactive checkpoints cover delivery modes, idle wake, persisted `customType`/`details`, hidden display after resume, independent `before_agent_start` messages, and in-run delivery without splitting tool call/result pairs. Missing: image content and custom-message `message_start/end`; noninteractive injection and live rendering remain incomplete. |
| 4 | Session replacement | `ctx.newSession`, `fork`, `switchSession`, `navigateTree`, `reload`, `ctx.shutdown`, `session_before_switch`, `session_before_fork` | **partial** | Real-host `newSession` and saved-file `switchSession` checkpoints complete the original command, run `withSession` on the replacement, persist callback appends, and preserve old-session bytes. Before-switch/fork cancellation checkpoints pass. Fork replacement and reload exist but remain unqualified. Missing: `newSession` `setup`/`parentSession`, `navigateTree`, `ctx.shutdown`. |
| 5 | Tool registration | `registerTool`; fields `name label description parameters execute promptSnippet promptGuidelines renderCall renderResult` | **implemented** | — |
| 6 | Tool definition fields | `outputSchema`, `prepareArguments`, `renderShell`, `annotations`, `defaultActive`, `executionMode`, `exposure`, `namespace`, `constrainedSampling`, `prepareLoadout` | **missing** | Accept Pi names (`outputSchema`, not `output_schema`); map onto the host tool definition; drop or honor each field explicitly. |
| 7 | Tool execution | `execute` result `content` (text and image), `details`, `isError`, `structuredContent`; `onUpdate` with `details`; `ctx.tools`, `ctx.executeTool` | **partial** | Accept `structuredContent` and image content; forward update details; implement `executeTool` on `tool_composition_v1`. |
| 8 | Runtime registration | `registerTool`, `on` and other registrations after the factory returns | **missing** | Dynamic tool registry exists in the host; route late registrations to it instead of refusing. |
| 9 | Commands | `registerCommand` (`description`, `handler`, `getArgumentCompletions`), `getCommands`, `ctx.waitForIdle`, `ctx.getSystemPromptOptions` | **partial** | `getSystemPromptOptions` missing. |
| 10 | Shortcuts and flags | `registerShortcut`, `registerFlag`, `getFlag` | **implemented** | — |
| 11 | Event bus | `pi.events` | **implemented** | — |
| 12 | Session entries | `appendEntry`, `setLabel`, `setSessionName`, `getSessionName`, `ctx.sessionManager` getters | **partial** | Session mirror is a bounded whole snapshot; large histories are refused. |
| 13 | Lifecycle events | `agent_start`, `agent_end`, `turn_start`, `turn_end`, `message_start/update/end`, `tool_execution_start/end`, `session_start`, `session_shutdown`, `session_info_changed` | **partial** | `agent_settled` fires once after `agent_end` on `turn/settled` (adapter test; not yet native). Add `agent_before_settle`, `tool_execution_update`; `turn_end` and `agent_before_settle` boundary results; `message_end` result. |
| 14 | Prompt and context events | `input` (`transform`, `handled`), `before_agent_start` (`systemPrompt`), `context`, `context_with_system` | **partial** | `input` results refused; `context_with_system` missing. |
| 15 | Compaction and tree | `ctx.compact`, `session_before_compact`, `session_compact`, `session_compact_failed`, `session_before_tree`, `session_tree` | **implemented** | — |
| 16 | Model and thinking | `setModel`, `setThinkingLevel`, `getThinkingLevel`, `ctx.model`, `ctx.thinkingLevel`, `ctx.scopedModels`, `ctx.modelRegistry`, `model_select`, `thinking_level_select` | **partial** | Add `setModel`, `setThinkingLevel`, `ctx.thinkingLevel`, `ctx.scopedModels`. |
| 17 | Providers | `registerProvider`, `unregisterProvider`, `registerVirtualModel`, `unregisterVirtualModel`, `before_provider_request`, `before_provider_headers`, `after_provider_response`, `provider_stream_event` | **partial** | Hooks done; registration missing. Host provider registry and stream transport exist; offer and bind them, then wire the adapter. |
| 18 | Active tools | `getActiveTools`, `getAllTools`, `setActiveTools` | **implemented** | — |
| 19 | Context facts | `cwd`, `hasUI`, `mode`, `signal`, `isIdle`, `hasPendingMessages`, `getSystemPrompt`, `getContextUsage`, `abort`, `isProjectTrusted`, `project_trust`, `pi.getSettings` | **partial** | Add `mode`, `abort`, `isProjectTrusted`, `project_trust`, `getSettings`. |
| 20 | Process execution | `pi.exec` | **partial** | Unix exact-argv execution uses the native effect broker and supervised process groups. Real-host tests cover safe controlled execution, exact approval/refusal, output/status, timeout and AbortSignal cancellation. Windows process-group supervision is missing; results remain bounded by negotiated protocol frame capacity. |
| 21 | MCP | `registerMcpServer`, `unregisterMcpServer`, `getMcpServers`, `mcp_servers_change` | **partial** | Synchronous session registry/change events and transient explicit-direct stdio routing reuse the resident `octet-mcp` manager. Native Rust App acceptance remains required; exposure, HTTP/credentials, expansion, Pi tool namespaces and progress-reset timeout gaps are explicit in the adapter README. |
| 22 | UI dialogs | `notify`, `confirm`, `input`, `select`, `editor`, `custom` | **partial** | `notify`/`confirm`/`select`/`input` load and run; native acceptance still fails for `custom` overlays with `onHandle`, dialog keys and dialog option countdowns (`pi_ui_contract_tests`, 2 of 5 pass). Deferred. |
| 23 | UI chrome | `setStatus`, `setWidget`, `setFooter`, `setHeader`, `setTitle`, `setWorkingIndicator`, `setWorkingMessage`, `setWorkingVisible`, `setHiddenThinkingLabel`, `theme`, `getTheme`, `getAllThemes`, `setTheme`, `getToolsExpanded`, `setToolsExpanded`, `onTerminalInput`, `ui_prompt_start/end` | **partial** | Status, widget, footer, header, theme and prompt events done; the rest missing. |
| 24 | Editor | `setEditorComponent`, `getEditorComponent`, `getEditorText`, `setEditorText`, `pasteToEditor`, `addAutocompleteProvider` | **partial** | `getEditorComponent` missing; `addAutocompleteProvider` implemented in `completions.mjs` but not exposed on `ctx.ui`. |
| 25 | Renderers | `renderCall`, `renderResult`, `renderShell`, `registerToolRenderer`, `registerMessageRenderer`, `registerEntryRenderer`, `registerMarkdownTransformer` | **partial** | Tool renderers render at width 80 without color; the four `register*Renderer`/transformer calls missing. |
| 26 | User bash | `user_bash` (`operations`, `result`) | **partial** | Observed only; results refused. |
| 27 | Helper imports | `@earendil-works/pi-coding-agent`, `pi-ai`, `pi-tui` public exports used by extensions | **partial** | 67 of Pi's 79 examples load on path A; 4 more load through the installed-Pi fallback (built-in tool factories, `pi-ai/compat`). See [release status](pi-compat-release-status.md). |

## Native session and persistence checkpoints

The integrated source passes these real Rust-host suites (the scripted local
provider replaces inference only):

```sh
cargo test -p octet-coding-agent --lib --locked --offline pi_session_replacement_tests -- --nocapture
# 5 passed: new-session and saved-session replacement, withSession, before-hook cancellation
cargo test -p octet-coding-agent --lib --locked --offline pi_messages_tests -- --nocapture
# 4 passed: persistence/resume, delivery ordering, before-agent messages and tool pairing
cargo test -p octet-coding-agent --lib --locked --offline pi_exec_contract_tests -- --nocapture
# 3 passed: exact argv, controlled-policy approval/refusal, timeout and cancellation
```

These qualify the listed interactive and Unix process behaviors, not all
members of rows 3–4 and 20. Those rows remain partial; the outstanding members
above are still required.

## Existing designs

`docs/design/extension-values-v1.md` slices A–E (typed values, `ResourceRef`,
`OperationDescriptor`, applicable-operation lookup, `BlobRef`) have host
features in source (`resource_refs_v1`, `operation_descriptors_v1`,
`bulk_objects_v1`). The SPICE demonstration (F) is deferred.

## Order

Rows 1–4, then 6–8, then the rest top to bottom.
