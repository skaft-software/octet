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
| 3 | Custom messages | `sendMessage(msg, {deliverAs, triggerTurn})`, `display`, `details`, `sendUserMessage(text, {deliverAs})`, `before_agent_start` `message` | **partial** | Real-host tests cover text/images, delivery modes, idle wake, persisted customType/details, hidden resume display, before-agent messages, public message_start/end payloads and intact tool pairs (9 pi_messages_tests). Noninteractive injection remains incomplete. |
| 4 | Session replacement | `ctx.newSession`, `fork`, `switchSession`, `navigateTree`, `reload`, `ctx.shutdown`, `session_before_switch`, `session_before_fork` | **partial** | Real-host newSession setup/parentSession, saved-file switchSession, writable withSession and before-switch/fork cancellation pass (7 pi_session_replacement_tests). Command-originated replacement/reload progress and retired-owner fencing also pass native regressions; navigateTree and shutdown remain incomplete. |
| 5 | Tool registration | `registerTool`; fields `name label description parameters execute promptSnippet promptGuidelines renderCall renderResult` | **done** | Native tool registration/execution is covered by pi_tools_surface_tests. Reviewed builtin replacement requires exact owner grants and builtin_tool_overrides_v1, with generation fencing, authoritative effects/replay and restoration after retirement. |
| 6 | Tool definition fields | `outputSchema`, `prepareArguments`, `renderShell`, `annotations`, `defaultActive`, `executionMode`, `exposure`, `namespace`, `constrainedSampling`, `prepareLoadout` | **partial** | Pi field names, outputSchema, prepareArguments, renderShell, defaultActive, constrained sampling and bounded prepareLoadout are mapped. Annotation/namespace observations are preserved. Non-direct exposure and non-sequential execution remain explicit refusals rather than silently weakened semantics. |
| 7 | Tool execution | `execute` result `content` (text and image), `details`, `isError`, `structuredContent`; `onUpdate` with `details`; `ctx.tools`, `ctx.executeTool` | **partial** | Native tests cover text/images, structured output, partial/final details, real ctx.tools and host-admitted ctx.executeTool outcomes. Optional undefined object members in tool details follow JSON omission; unsupported values and arrays remain strict. Complete execution-event/parallel parity remains open. |
| 8 | Runtime registration | `registerTool`, `on` and other registrations after the factory returns | **partial** | Late tools use the native dynamic registry and acknowledged rollback; late observer/render registrations invalidate the appropriate consumer. Other registrations remain constrained by captured capabilities; arbitrary manifest widening is not supported. |
| 9 | Commands | `registerCommand` (`description`, `handler`, `getArgumentCompletions`), `getCommands`, `ctx.waitForIdle`, `ctx.getSystemPromptOptions` | **partial** | Argument completions, getCommands, waitForIdle and bounded getSystemPromptOptions exist. Completion quote/cursor edits outside the native wire profile refuse; complete completion-UI parity remains unqualified. |
| 10 | Shortcuts and flags | `registerShortcut`, `registerFlag`, `getFlag` | **implemented** | — |
| 11 | Event bus | `pi.events` | **implemented** | — |
| 12 | Session entries | `appendEntry`, `setLabel`, `setSessionName`, `getSessionName`, `ctx.sessionManager` getters | **partial** | Session mirror is a bounded whole snapshot; large histories are refused. |
| 13 | Lifecycle events | `agent_start`, `agent_end`, `turn_start`, `turn_end`, `message_start/update/end`, `tool_execution_start/end`, `session_start`, `session_shutdown`, `session_info_changed` | **partial** | Bounded message observations and agent_end.messages use committed entry-linked model/usage/stop facts; missing facts are not invented. Actual model-turn hooks, agent_settled, startup/new/resume/fork reasons and shutdown fences are implemented. Native non-resource startup tests qualify startup-before-command and exactly-once headless behavior; failed startup workers block admission. Recoverable observer errors are grouped; veto/cancel/persistence failures remain fail-closed. Exact tool-execution timing and boundary transformations remain incomplete. |
| 14 | Prompt and context events | `input` (`transform`, `handled`), `before_agent_start` (`systemPrompt`), `context`, `context_with_system` | **partial** | Native input transform/handled sequencing is qualified; before-agent applied-system and context/context_with_system projections exist. Opaque replay is provenance-bound; unsupported projections refuse. Full provider-context parity remains incomplete. |
| 15 | Compaction and tree | `ctx.compact`, `session_before_compact`, `session_compact`, `session_compact_failed`, `session_before_tree`, `session_tree` | **implemented** | — |
| 16 | Model and thinking | `setModel`, `setThinkingLevel`, `getThinkingLevel`, `ctx.model`, `ctx.thinkingLevel`, `ctx.scopedModels`, `ctx.modelRegistry`, `model_select`, `thinking_level_select` | **partial** | Native tests cover model facts, scoped views and idle setModel/setThinkingLevel with live command ownership. Missing provider/model facts remain explicit; broad provider/auth catalog parity is not established. |
| 17 | Providers | `registerProvider`, `unregisterProvider`, `registerVirtualModel`, `unregisterVirtualModel`, `before_provider_request`, `before_provider_headers`, `after_provider_response`, `provider_stream_event` | **partial** | Explicit custom-provider registration, replacement, selection and streaming pass native tests. Builtin provider inheritance, virtual models, provider_stream_event and provider OAuth remain unsupported. The original Anthropic-attribution callback needs raw credentials; existing trust/leases do not authorize exporting them. |
| 18 | Active tools | `getActiveTools`, `getAllTools`, `setActiveTools` | **done** | Real App/adapter startup hooks and commands read authoritative catalogs and acknowledge active-tool selections, including defaultActive and late registration (pi_tools_surface_tests). |
| 19 | Context facts | `cwd`, `hasUI`, `mode`, `signal`, `isIdle`, `hasPendingMessages`, `getSystemPrompt`, `getContextUsage`, `abort`, `isProjectTrusted`, `project_trust`, `pi.getSettings` | **partial** | Mode, settings and available host facts are mapped. isProjectTrusted requires an authoritative snapshot; ctx.abort has no root-run abort contract and project_trust events remain unsupported. Missing facts are not invented. |
| 20 | Process execution | `pi.exec` | **partial** | Unix exact-argv execution uses the native effect broker and supervised process groups. Real-host tests cover safe controlled execution, exact approval/refusal, output/status, timeout and AbortSignal cancellation. Windows process-group supervision is missing; results remain bounded by negotiated protocol frame capacity. |
| 21 | MCP | `registerMcpServer`, `unregisterMcpServer`, `getMcpServers`, `mcp_servers_change` | **partial** | Synchronous session registry/change events and transient explicit-direct stdio routing reuse the resident `octet-mcp` manager. Three real Rust App cases pass registration/replacement, actual tool execution/persistence, removal/owner cleanup, headless startup and missing-resident refusal. Exposure, HTTP/credentials, expansion, Pi tool namespaces and progress-reset timeout gaps remain explicit in the adapter README. |
| 22 | UI dialogs | `notify`, `confirm`, `input`, `select`, `editor`, `custom` | **partial** | Real Pi UI contract tests now pass (7/7), including custom onHandle overlays, dialog keys/countdowns and foreground notification refusal. Complete public UI behavior outside these bounded native cases remains unqualified. |
| 23 | UI chrome | `setStatus`, `setWidget`, `setFooter`, `setHeader`, `setTitle`, `setWorkingIndicator`, `setWorkingMessage`, `setWorkingVisible`, `setHiddenThinkingLabel`, `theme`, `getTheme`, `getAllThemes`, `setTheme`, `getToolsExpanded`, `setToolsExpanded`, `onTerminalInput`, `ui_prompt_start/end` | **partial** | Native loader/title/tool-disclosure controls and bounded semantic desktop notification intent pass tests. `onTerminalInput` now runs a bounded pre-native lane: listeners see the raw terminal spelling in registration order before any native owner, can consume or replace input, and the whole chain is capped at 50 ms per event (Pi blocks indefinitely). A listener that stops answering is latched off with one diagnostic and that event is delivered unchanged; a throwing listener is reported once and keeps its subscription. `Ctrl+G`/`Ctrl+D` and input past the existing 256-byte bound stay native. Real-host acceptance in `pi_ui_contract_tests`; the wire is `terminal_input_intercept_v1`. Imported Pi themes initialize native appearance and Pi helper palettes; explicit native selections win, but live native-to-Pi palette synchronization is unavailable. |
| 24 | Editor | `setEditorComponent`, `getEditorComponent`, `getEditorText`, `setEditorText`, `pasteToEditor`, `addAutocompleteProvider` | **partial** | Native editor factory identity/checkpoints pass, getEditorComponent and addAutocompleteProvider are exposed, and original rainbow paced/burst rescue PTYs pass. Settings-only editor history stays native. Border-color translation and complete arbitrary-editor parity remain open; custom-editor slash-menu work is excluded from this candidate. |
| 25 | Renderers | `renderCall`, `renderResult`, `renderShell`, `registerToolRenderer`, `registerMessageRenderer`, `registerEntryRenderer`, `registerMarkdownTransformer` | **done** | Real App/adapter acceptance covers tool call/result, custom message, private entry and Markdown presentation, including controlled native read execution, persistence, invalidation, resize and active Tern. Bounded driver/source/cache tests cover owner/geometry fences, safe SGR and fallback. Canonical source and persistence remain unchanged; original-extension corpus qualification is separate. |
| 26 | User bash | `user_bash` (`operations`, `result`) | **partial** | Observed only; results refused. |
| 27 | Helper imports | `@earendil-works/pi-coding-agent`, `pi-ai`, `pi-tui` public exports used by extensions | **partial** | 67 of Pi's 79 examples load on path A; 4 more load through the installed-Pi fallback (built-in tool factories, `pi-ai/compat`). See [release status](pi-compat-release-status.md). |

## Native session and persistence checkpoints

The integrated source passes these real Rust-host suites (the scripted local
provider replaces inference only):

```sh
cargo test -p octet-coding-agent --lib --locked --offline pi_session_replacement_tests -- --nocapture
# 7 passed: replacement, setup/parentSession, withSession and before-hook cancellation
cargo test -p octet-coding-agent --lib --locked --offline pi_messages_tests -- --nocapture
# 9 passed: text/images, public message events, persistence/resume, delivery and tool pairing
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
