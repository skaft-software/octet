# Pi 0.84.4 compatibility ledger

> **Archived Pi bridge evidence — non-shipping, not a release gate.**
> Preserved from the local pre-reduction 0.8.0 candidate. The Pi execution bridge
> and `octet pi` command family are removed; commands, tests, “current” claims,
> and release requirements below describe the historical implementation only.
> Referenced bridge source/fixtures are no longer installed or executable here.
> The JSON profiles are preserved byte-for-byte as inert evidence, not runtime
> configuration. No receipt here qualifies the reduced RC or a published release.
> Current [Pi inventory/import](../../pi-migration.md) and native providers are
> separate from Pi extension execution.

This is the human view of the canonical machine-readable [0.84.4 ledger](profiles/0.84.4.ledger.json). It targets the public API exported by `@earendil-works/pi-coding-agent@0.84.4` and `@earendil-works/pi-tui@0.84.4`; private `dist/` imports are outside the target.

The *runtime* selector accepts `@earendil-works/pi-coding-agent` `>=0.84.4 <0.86.0` so a newer patch/minor in the same family is not refused by string equality. The *conformance profile* remains pinned to `0.84.4`: every row, fixture, and claim below is validated against that revision only, and no newer runtime has passed an integrity-verified campaign.

## Claim and status vocabulary

**Current claim:** `dogfood_conformance`. The executable fixture suite proves declared bridge behavior and explicit safe divergences. It does **not** claim byte-for-byte Pi TUI, full provider/OAuth, or full real-runtime equivalence.

- **passing** — the declared host-visible bridge behavior is exercised.
- **safe divergence** — behavior is reduced or rejected visibly, with the named dogfood decision below; no call is silently accepted as equivalent.
- **known dogfood bug** — reserved for a bounded, named defect with the same release decision requirement.

All non-passing rows below use decision `pi-0.84.4-dogfood-explicit-safe-divergence`: current dogfood branch only; release approval is required before a broader equivalence claim.

## API 0.3 provider bridge

API `0.2` remains the default and does not gain provider behavior. The
constrained provider bridge is selected only by `octet pi install SOURCE
--api-version 0.3`. Its manifest contributes `providers` and one fixed
aggregate Pi-tool dispatcher, while omitting legacy commands, UI, context,
notification, confirmation, process, and network contributions.

The API `0.3` provider rows below remain **safe divergences**, not `passing`:
they are exercised by checked-in deterministic fake-Pi protocol fixtures. The
bridge sends secret-free catalog declarations and bounded semantic requests;
octet retains credentials and authorization leases. Endpoint, header, API-key,
transport, callback, and OAuth payload authority are rejected or withheld.
The fixture coverage is not a real-runtime provider-parity claim.

## Optional API 0.2 UI fixture coverage

The public-surface rows and canonical machine ledger below describe baseline
fixtures without optional UI features. They are not assertions that an optional
host-admitted surface has been exercised against real Pi. Their statuses and
pinned inventory remain unchanged.

The bridge additionally accepts explicit legacy `semantic_ui`, `editor_handoff`,
`autocomplete` and resize observation features, as described in the
[UI handoff contract](README.md#optional-api-02-ui-handoff). Bounded text widgets,
header/footer projections, editor acknowledgements and suffix completions now
have separate actual-bridge fixtures in `tests/test_bridge_ui.py`; these do not
replace the baseline rejection fixtures or the unchanged-source full gate.
Raw input handlers, replacement editors and arbitrary terminal components remain
unsupported. Both API `0.3` provider mode and the private projection helper reject
legacy UI admission. The helper describes only legacy `0.2` features, not a
canonical `0.3` schema or capability grant.

See the [candidate qualification](../../qualification/pi-ui-current-candidate.md)
for exact fixture coverage and outstanding generated-link/Rust-host/native gates.
The deferred widget/editor statements in the baseline tables and plan-mode
journeys below retain that no-optional-UI scope, not a full parity claim.

## Additional bounded bridge regressions

`tests/test_bridge_ui.py` also exercises Pi select/confirm/input `signal` and
`timeout` dismissal, pre-aborted dialogs, parent cancellation, owner settlement,
and selection labels with numeric prefixes. Timed-out or dismissed dialogs send
host cancellation and return Pi's false/undefined defaults, never approval. The
host input/confirmation UI is still not a Pi selector/countdown component.

`tests/test_bridge_protocol.py` verifies directory packages containing multiple
unchanged entrypoints and refuses entrypoints outside the source fingerprint's
file domain before import. Pi's public package resolver, not a vendored manifest
or glob implementation, determines the selected files. Exact path-set validation
replaces the invalid one-loaded-extension-per-directory assumption.

Public Pi argument validation now runs after preparation and after interception,
before execution on both tool wires. The API `0.3` dispatcher additionally calls
Pi's tool-call interception. Adversarial fixtures assert zero execute effects for
non-coercible values, additional properties, invalid preparation and invalid hook
mutation, observed through a marker file each fixture tool writes only when its
execute callback actually runs; Pi number-to-string coercion is retained, not
rejected as an invalid raw value. This imports the selected runtime's public
`pi-ai` export.

### Negotiated tool-result contract (usage and termination)

Pi declares `usage` and `terminate` on an executed tool result. Carrying them is a
host kernel service (durable tool-usage accounting and the finalized-batch
unanimous termination rule), so the bridge negotiates two independent optional
features with the same names on both wires — `tool_result_usage` and
`tool_result_termination` — and selects each only from the host's own offer.

* With the feature selected: the bridged tool result carries a typed
  `usage` record with the kernel's native tool-usage counters one for one —
  `input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_write_tokens`,
  `total_tokens`, plus optional `cache_write_1h_tokens` and `reasoning_tokens` —
  and an optional `terminate` boolean. `details` keeps its original shape and is
  never used to smuggle usage.
* Pi reports USD `cost` on the same Usage record. An all-zero cost is Pi's own
  unpriced encoding and is accepted (nothing is lost); a non-zero cost fails
  explicitly, because the kernel's typed tool usage carries token counters only
  and silently discarding a billed amount would be a false accounting claim.
  Carrying cost needs an exact bounded decimal amount type and its aggregation in
  the kernel first.
* With the feature absent: a Pi result that carries the field fails explicitly
  (`invalid params` on the API `0.3` envelope, a named error on API `0.2`). It is
  neither dropped nor relabelled as generic metadata, and the bridge never
  advertises the field to a host that did not offer it.
* `usage` follows Pi's own merge rule: a `tool_result` hook may replace it
  (`hook.usage ?? result.usage`), absent hook usage preserves the executed usage,
  and exactly one final record is emitted. Termination comes from the executed
  result only — Pi's `ToolResultEventResult` declares no termination mutation — so
  a hook that tries to change it is an unrepresentable mutation that fails.
* The current octet host does **not** offer either feature. Until its decoder and
  durable consumer land, this profile therefore refuses those fields rather than
  claiming tool-usage accounting or batch termination parity.

### Tool-definition projections

Pi declares more on a tool definition than the API `0.2` tool wire carries. The
bridge never sends an undeclared wire field, because the host decodes the tool
definition with an exact field set:

* `promptSnippet` and `promptGuidelines` are projected into the model-facing
  description, using Pi's own normalization (one-line snippet, trimmed and
  de-duplicated guideline bullets). Pi renders the snippet in its system-prompt
  tool list and the guidelines as prompt bullets; octet exposes the same text
  through the tool schema.
* `executionMode: "sequential"`, which makes Pi serialize a whole tool batch, is
  enforced by a bridge execution lane: while any registered Pi tool declares it,
  bridged Pi tool executions never overlap. The host already classifies extension
  tool calls as batch barriers, so this lane is defence in depth rather than the
  only serialization.
* `constrainedSampling` that Pi itself treats as a hard requirement
  (`strict: "require"`, or a grammar variant) fails initialization explicitly,
  because the octet host owns provider requests and serves no per-tool sampling
  contract. A `strict: "prefer"` request is accepted with a startup diagnostic:
  calls are produced unconstrained and still validated before execution.

Still open on this profile: the host result wire and durable native tool-usage
accounting (including an exact bounded decimal cost amount), same-name concurrent
hook/call FIFO identity, and the remaining
session/control/root/custom-entry/label/model/thinking/scoped-model/active-tools/
compaction/idle-queue surfaces recorded below. A busy `waitForIdle` errors rather
than claiming to wait.

These are deterministic bridge regressions, not a new runtime qualification.
The inspected `8a7b0c03dfb702663acafb6dc29f8acaa4ffe391` source is Pi `0.85.1`;
the pinned `0.84.4` profile and all baseline ledger statuses remain unchanged.
The optional typed host `event_bus` offer is recognized but not selected or
translated into arbitrary Pi cross-process event authority.

## Wave-1 negotiated power-parity surfaces

The Wave-1 rows in the tables below that are marked **passing** are driven by
`tests/test_bridge_wave1.py` against the real bridge. Every one of them is
gated by a negotiated API `0.2` feature — `composer`, `shortcuts`,
`session_entries`, `message_injection`, `lifecycle_events_v2`, `active_tools` —
and the API version remains `0.2`. A host that offers none of them still sees
the declared refusal, which the same suite pins in the feature-absent tests, and
the baseline public-surface probes keep reporting an explicit refusal.

The fixtures prove the observable contract rather than a declaration: the
extension-visible value round trip, the exact owner-scoped request envelope
(`parent_request_id` plus only the contract fields plus an optional shaped
`resource_owner`, never a fabricated owner; the settled-parent admission that
field exists for is a host contract and is not yet bridge-observable), the host
entry id returned to `pi.appendEntry`, `bounds_exceeded`/`invalid_request`
local refusals before a frame is written, `not_foreground_owner` refusals from
the host that stay typed at the Pi surface, and a single coalesced
`message/updated` fan-out that opens no per-delta round trip. `shortcut/trigger`
for a refused registration fails closed and never runs a handler.

`ui_context.editor` stays a **safe divergence**: `ctx.ui.editor` is acknowledged
through the negotiated `editor_handoff` surface (and refused without it), but
the edited text stays host-owned and is never fabricated. Themes, focused
remote components, replacement editors, session replacement, `ctx.reload`,
and `ctx.shutdown` stay explicitly refused.

## Wave-1 host application status

Every Wave-1 row is now split by what the **shipped host** actually does, and the
tables above reflect that split exactly:

- **Applied end to end:** the composer trio (`composer/get|set|insert` against the
  real frontend composer), `shortcut/register` plus `shortcut/trigger` dispatch
  (host keymap entries, with reserved host bindings refused visibly),
  `session/append_entry` and `session/set_label` (durable, extension-scoped,
  never model-visible), `session/set_name`, `session/send_user_message` (queued
  on the real turn path), `tools/set_active` (narrowing only, enforced at the
  host-policed tool surface and at dispatch; unknown or policy-excluded names are
  refused with no state change), and every `lifecycle_events_v2` notification the
  coding agent produces — `message/started|updated|settled`,
  `compaction/started|settled|failed`, `dialog/started|settled`,
  `session/info_changed`, `model/selected`, `reasoning/selected`, `bash/user`.
  These rows are **passing**.
- **Still an explicit safe divergence:** `pi.sendMessage` for the `assistant` and
  `system` roles. octet injects into the real turn path only for user messages;
  fabricating an assistant or system turn would falsify transcript provenance and
  usage accounting, so the role is refused with the typed `unsupported_feature`
  rather than silently accepted. `flags` projection and the full editor/widget
  transport stay deferred for the same reason: they are absent capabilities, not
  hidden ones.

## Executable inventory

`python3 extensions/octet-pi-compat/conformance.py --check --json` validates the 118 public-surface rows, all 78 official extension entries (69 files and 9 directories), all 33 Pi TUI audit rows, the six plan-mode journeys, fixture links, and the raw-byte profile integrity sidecar.

`--check` reports `real_runtime: not_supplied` unless a separate full gate is run. A developer smoke with `OCTET_PI_REAL_PACKAGE` is useful diagnosis, not integrity evidence.

### Public extension surface

#### Extension events

| Pi surface | Status | Fixture | Declared behavior |
| --- | --- | --- | --- |
| `project_trust` | safe divergence | `events:project_trust` | Event registration is diagnosed explicitly because project_trust is not emitted by the bounded bridge. |
| `resources_discover` | safe divergence | `events:resources_discover` | Event registration is diagnosed explicitly because resources_discover is not emitted by the bounded bridge. |
| `session_start` | safe divergence | `events:session_start` | Emitted from session/started with a host-derived reason. |
| `session_info_changed` | passing | `events:session_info_changed` | Emits the host `session/info_changed` notification as the Pi `session_info_changed` event when `lifecycle_events_v2` is negotiated. |
| `session_before_switch` | safe divergence | `events:session_before_switch` | Event registration is diagnosed explicitly because session_before_switch is not emitted by the bounded bridge. |
| `session_before_fork` | safe divergence | `events:session_before_fork` | Event registration is diagnosed explicitly because session_before_fork is not emitted by the bounded bridge. |
| `session_before_compact` | passing | `events:session_before_compact` | Emits the host `compaction/started` notification as the Pi `session_before_compact` event when `lifecycle_events_v2` is negotiated. |
| `session_compact` | passing | `events:session_compact` | Emits the host `compaction/settled` notification as the Pi `session_compact` event when `lifecycle_events_v2` is negotiated. |
| `session_compact_failed` | passing | `events:session_compact_failed` | Emits the host `compaction/failed` notification as the Pi `session_compact_failed` event when `lifecycle_events_v2` is negotiated. |
| `session_shutdown` | safe divergence | `events:session_shutdown` | Emitted once from settlement or shutdown with a synthetic reason. |
| `session_before_tree` | safe divergence | `events:session_before_tree` | Event registration is diagnosed explicitly because session_before_tree is not emitted by the bounded bridge. |
| `session_tree` | safe divergence | `events:session_tree` | Event registration is diagnosed explicitly because session_tree is not emitted by the bounded bridge. |
| `context` | safe divergence | `events:context` | Bounded additions become octet context; canonical history replacement remains host-owned. |
| `before_provider_request` | safe divergence | `events:before_provider_request` | API `0.3` only: bounded canonical semantic request transforms are applied; envelope-level endpoint, credential, header, transport, callback, and OAuth authority are rejected. |
| `before_provider_headers` | safe divergence | `events:before_provider_headers` | API `0.3` explicitly rejects this hook because request and response headers remain host-owned authority. |
| `after_provider_response` | safe divergence | `events:after_provider_response` | API `0.3` only: receives reduced status with an empty header object; raw response headers and body remain host-owned. |
| `before_agent_start` | safe divergence | `events:before_agent_start` | Bounded system/message additions become host context suffixes. |
| `agent_start` | safe divergence | `events:agent_start` | Emitted from turn/started with the bridge lifecycle payload. |
| `agent_end` | safe divergence | `events:agent_end` | Emitted after response settlement with reduced messages. |
| `agent_settled` | safe divergence | `events:agent_settled` | Emitted after the synthetic agent_end event. |
| `ui_prompt_start` | safe divergence | `events:ui_prompt_start` | Emits the host `dialog/started` notification as the Pi `ui_prompt_start` event when `lifecycle_events_v2` is negotiated. The octet coding agent has no producer for this notification yet, so the event does not fire on this release. |
| `ui_prompt_end` | safe divergence | `events:ui_prompt_end` | Emits the host `dialog/settled` notification as the Pi `ui_prompt_end` event when `lifecycle_events_v2` is negotiated. The octet coding agent has no producer for this notification yet, so the event does not fire on this release. |
| `turn_start` | safe divergence | `events:turn_start` | Emitted with a synthetic turn index and timestamp. |
| `turn_end` | safe divergence | `events:turn_end` | Emitted with reduced tool-result and message history. |
| `message_start` | safe divergence | `events:message_start` | Emits the host `message/started` notification as the Pi `message_start` event when `lifecycle_events_v2` is negotiated (the host coalescer exists and is unit-tested). The octet coding agent has no producer for this notification yet, so the event does not fire on this release. |
| `message_update` | safe divergence | `events:message_update` | Forwards one coalesced `message/updated` batch once, in host order, as the Pi `message_update` event; the bridge opens no per-delta round trip. the octet coding agent has no producer for this notification yet, so the event does not fire on this release. |
| `message_end` | safe divergence | `events:message_end` | Emits the host `message/settled` notification as the Pi `message_end` event when `lifecycle_events_v2` is negotiated. The octet coding agent has no producer for this notification yet, so the event does not fire on this release. |
| `tool_execution_start` | safe divergence | `events:tool_execution_start` | Emitted from host tool/started lifecycle. |
| `tool_execution_update` | safe divergence | `events:tool_execution_update` | Pi partial results route through bounded octet progress. |
| `tool_execution_end` | safe divergence | `events:tool_execution_end` | Emitted from host tool/settled lifecycle with reduced result details. |
| `model_select` | passing | `events:model_select` | Emits the host `model/selected` notification as the Pi `model_select` event when `lifecycle_events_v2` is negotiated. |
| `thinking_level_select` | passing | `events:thinking_level_select` | Emits the host `reasoning/selected` notification as the Pi `thinking_level_select` event when `lifecycle_events_v2` is negotiated. |
| `user_bash` | safe divergence | `events:user_bash` | Emits the host `bash/user` notification as the Pi `user_bash` event when `lifecycle_events_v2` is negotiated. The octet coding agent has no producer for this notification yet, so the event does not fire on this release. |
| `input` | safe divergence | `events:input` | Event registration is diagnosed explicitly because input is not emitted by the bounded bridge. |
| `tool_call` | safe divergence | `events:tool_call` | Blocks and Pi-tool argument preparation are preserved; unrepresentable mutation fails. |
| `tool_result` | safe divergence | `events:tool_result` | Pi-tool content/details/error transforms are preserved and hook usage applies; executed usage/termination cross only under their negotiated result features, and an unnegotiated result field fails explicitly instead of being dropped. |

#### `ExtensionAPI`

| Pi surface | Status | Fixture | Declared behavior |
| --- | --- | --- | --- |
| `on` | safe divergence | `extension_api:on` | Supported events are registered; unavailable events emit a startup diagnostic. |
| `registerTool` | safe divergence | `extension_api:registerTool` | Initial tool registration and execution use the public Pi runner; promptSnippet/promptGuidelines are projected into the model-facing description, executionMode: "sequential" is enforced by a bridge execution lane, and a constrainedSampling requirement fails closed. |
| `registerCommand` | safe divergence | `extension_api:registerCommand` | Initial commands become native octet commands when runtime_commands is negotiated. |
| `registerShortcut` | passing | `extension_api:registerShortcut` | Registers one runtime shortcut through `shortcut/register` when `shortcuts` is negotiated; `shortcut/trigger` dispatches only an admitted registration. |
| `registerFlag` | safe divergence | `extension_api:registerFlag` | Pi exposes flags only after loading extension code, while octet discovers trusted API `0.3` manifest flags before startup; bridge registration remains diagnosed. |
| `getFlag` | safe divergence | `extension_api:getFlag` | Returns Pi runtime/default values; the API `0.2` bridge cannot receive octet's API `0.3` pre-start invocation values. |
| `registerMessageRenderer` | safe divergence | `extension_api:registerMessageRenderer` | Remote component rendering is rejected explicitly. |
| `registerMarkdownTransformer` | safe divergence | `extension_api:registerMarkdownTransformer` | Transcript mutation is rejected explicitly. |
| `registerEntryRenderer` | safe divergence | `extension_api:registerEntryRenderer` | Remote component rendering is rejected explicitly. |
| `sendMessage` | safe divergence | `extension_api:sendMessage` | Refuses `pi.sendMessage` for the `assistant` and `system` roles with a typed `unsupported_feature`: fabricating an assistant or system turn would falsify transcript provenance and usage accounting. The user-message path (`pi.sendUserMessage`) is live. |
| `sendUserMessage` | passing | `extension_api:sendUserMessage` | Injects one bounded user message through `session/send_user_message` when `message_injection` is negotiated. |
| `appendEntry` | passing | `extension_api:appendEntry` | Appends one bounded durable entry through `session/append_entry` when `session_entries` is negotiated and returns the host entry id. The entry is durable, extension-scoped, and never model-visible. |
| `setSessionName` | passing | `extension_api:setSessionName` | Sets the bounded session name through `session/set_name` when `session_entries` is negotiated. |
| `getSessionName` | safe divergence | `extension_api:getSessionName` | Returns the latest host session-name snapshot. |
| `setLabel` | passing | `extension_api:setLabel` | Labels one durable entry through `session/set_label` when `session_entries` is negotiated. The label is durable and replaceable; an unknown entry id is refused with no state change. |
| `exec` | safe divergence | `extension_api:exec` | Runs only inside the explicitly trusted Pi extension process, never through octet bash. |
| `getActiveTools` | safe divergence | `extension_api:getActiveTools` | Reports bridge-local Pi tools rather than the full octet policy. |
| `getAllTools` | safe divergence | `extension_api:getAllTools` | Reports bridge-local Pi tool information. |
| `setActiveTools` | passing | `extension_api:setActiveTools` | Replaces the active tool set through `tools/set_active` when `active_tools` is negotiated. The change can only narrow the host-policed surface; unknown or policy-excluded names are refused with no state change. |
| `getCommands` | safe divergence | `extension_api:getCommands` | Reports commands registered by the Pi runner. |
| `setModel` | safe divergence | `extension_api:setModel` | Model mutation remains host-owned and errors explicitly. |
| `getThinkingLevel` | safe divergence | `extension_api:getThinkingLevel` | Derives a read-only level from the host reasoning snapshot. |
| `setThinkingLevel` | safe divergence | `extension_api:setThinkingLevel` | Reasoning mutation remains host-owned and errors explicitly. |
| `registerProvider` | safe divergence | `extension_api:registerProvider` | API `0.3` only: secret-free declarations, supported model protocols, catalog publication/update, host authorization status/refresh, canonical streaming, cancellation, and replacement cleanup are bridged. Credentials, endpoints, headers, transports, callbacks, OAuth payloads, and unsupported mutations are rejected or retained by octet. |
| `unregisterProvider` | safe divergence | `extension_api:unregisterProvider` | API `0.3` only: removes local routability, cancels active streams, requests host authorization revocation, and queues host catalog cleanup. Failed host acknowledgement remains inspectable and fail-closed. |
| `events.emit` | safe divergence | `extension_api:events.emit` | Event bus is shared only by sources in the same bridge process. |
| `events.on` | safe divergence | `extension_api:events.on` | Event bus is shared only by sources in the same bridge process. |

#### `ExtensionUIContext`

| Pi surface | Status | Fixture | Declared behavior |
| --- | --- | --- | --- |
| `select` | safe divergence | `ui_context:select` | Uses bounded text input rather than a Pi selector component. |
| `confirm` | safe divergence | `ui_context:confirm` | Uses octet confirmation requests. |
| `input` | safe divergence | `ui_context:input` | Uses octet bounded single-line input requests. |
| `notify` | passing | `ui_context:notify` | Emits a declared octet notification without writing protocol stdout. |
| `onTerminalInput` | safe divergence | `ui_context:onTerminalInput` | Raw terminal ownership stays in octet and errors explicitly. |
| `setStatus` | safe divergence | `ui_context:setStatus` | Publishes one semantic octet status contribution. |
| `setWorkingMessage` | safe divergence | `ui_context:setWorkingMessage` | Working-label ownership stays in octet and errors explicitly. |
| `setWorkingVisible` | safe divergence | `ui_context:setWorkingVisible` | Working-loader visibility stays in octet and errors explicitly. |
| `setWorkingIndicator` | safe divergence | `ui_context:setWorkingIndicator` | Custom loading frames are rejected explicitly. |
| `setHiddenThinkingLabel` | safe divergence | `ui_context:setHiddenThinkingLabel` | Hidden-thinking label mutation is rejected explicitly. |
| `setWidget` | safe divergence | `ui_context:setWidget` | Widget transport is rejected explicitly; plain-text host projection is deferred. |
| `setFooter` | safe divergence | `ui_context:setFooter` | Remote footer components are rejected explicitly. |
| `setHeader` | safe divergence | `ui_context:setHeader` | Remote header components are rejected explicitly. |
| `setTitle` | safe divergence | `ui_context:setTitle` | Terminal title ownership stays in octet and errors explicitly. |
| `custom` | safe divergence | `ui_context:custom` | Focused remote components are rejected explicitly. |
| `pasteToEditor` | passing | `ui_context:pasteToEditor` | Inserts text at the host composer cursor through `composer/insert` when `composer` is negotiated; the text is bounded and owner-scoped. |
| `setEditorText` | passing | `ui_context:setEditorText` | Replaces the host composer text through `composer/set` when `composer` is negotiated; the text is bounded and owner-scoped. |
| `getEditorText` | passing | `ui_context:getEditorText` | Returns the host composer text through `composer/get` when `composer` is negotiated; the snapshot stays bounded and owner-scoped. |
| `editor` | safe divergence | `ui_context:editor` | Acknowledges the host editor handoff by seeding the requested prefill and focusing the host editor through the negotiated `editor_handoff` surface, and stays explicitly refused without it; the edited text stays host-owned and is never returned. |
| `addAutocompleteProvider` | safe divergence | `ui_context:addAutocompleteProvider` | Autocomplete mutation is rejected explicitly. |
| `setEditorComponent` | safe divergence | `ui_context:setEditorComponent` | Remote editor components are rejected explicitly. |
| `getEditorComponent` | safe divergence | `ui_context:getEditorComponent` | Remote editor components are rejected explicitly. |
| `theme` | safe divergence | `ui_context:theme` | Text/style helpers preserve text while stripping Pi styling. |
| `getAllThemes` | safe divergence | `ui_context:getAllThemes` | Pi theme inventory is rejected explicitly. |
| `getTheme` | safe divergence | `ui_context:getTheme` | Pi theme lookup is rejected explicitly. |
| `setTheme` | safe divergence | `ui_context:setTheme` | Pi theme mutation is rejected explicitly. |
| `getToolsExpanded` | safe divergence | `ui_context:getToolsExpanded` | Transcript disclosure state is rejected explicitly. |
| `setToolsExpanded` | safe divergence | `ui_context:setToolsExpanded` | Transcript disclosure mutation is rejected explicitly. |

#### Context surfaces

| Pi surface | Status | Fixture | Declared behavior |
| --- | --- | --- | --- |
| `ui` | safe divergence | `context:ui` | Provides the bounded bridge UI context. |
| `mode` | safe divergence | `context:mode` | Always Pi RPC mode rather than an interactive Pi TUI. |
| `hasUI` | safe divergence | `context:hasUI` | True only for bridged dialogs, not arbitrary Pi components. |
| `cwd` | safe divergence | `context:cwd` | Uses the canonical octet workspace. |
| `sessionManager` | passing | `context:sessionManager` | Supplies a read-only foreground session snapshot (session id, host-assigned name, model, reasoning, bounded active skills, cwd) through `context/session_manager` when `session_context` is negotiated. A missing session or workspace is refused with a typed error rather than answered with a fabricated placeholder. |
| `modelRegistry` | safe divergence | `context:modelRegistry` | Credential/model registry access is rejected explicitly. |
| `model` | safe divergence | `context:model` | A Pi Model object is not reconstructed from a octet model id. |
| `scopedModels` | safe divergence | `context:scopedModels` | Returns an empty read-only snapshot. |
| `thinkingLevel` | safe divergence | `context:thinkingLevel` | Derives from the host reasoning snapshot. |
| `isIdle` | safe divergence | `context:isIdle` | Tracks the bridged turn lifecycle only. |
| `isProjectTrusted` | safe divergence | `context:isProjectTrusted` | Conservatively returns false because octet trust is not projected. |
| `signal` | safe divergence | `context:signal` | Binds to the active octet request cancellation signal. |
| `abort` | safe divergence | `context:abort` | Cancels the active bridged request. |
| `hasPendingMessages` | passing | `context:hasPendingMessages` | Reports the number of queued follow-up messages the foreground shell has not yet admitted, observed from the exact queue `session/send_user_message` feeds, through `context/pending_messages` when `session_context` is negotiated. |
| `shutdown` | safe divergence | `context:shutdown` | Extension code cannot terminate the host and errors explicitly. |
| `getContextUsage` | safe divergence | `context:getContextUsage` | Returns unknown without a bounded host usage snapshot. |
| `compact` | safe divergence | `context:compact` | Compaction is host-owned and errors explicitly. |
| `getSystemPrompt` | passing | `context:getSystemPrompt` | Discloses the exact composed system prompt through `context/system_prompt` when `system_prompt_read` is negotiated AND the extension manifest declares `capabilities.system_prompt = true`; the feature is not offered to any other extension and an undeclared echo fails negotiation. An over-bound prompt is refused as `bounds_exceeded` rather than truncated. |
| `getSystemPromptOptions` | safe divergence | `context:getSystemPromptOptions` | Returns only canonical cwd. |
| `waitForIdle` | safe divergence | `context:waitForIdle` | Returns at an observed idle boundary; a busy owner errors because the host idle-wait service is unavailable. |
| `newSession` | safe divergence | `context:newSession` | Session replacement is rejected explicitly. |
| `fork` | safe divergence | `context:fork` | Session replacement is rejected explicitly. |
| `navigateTree` | safe divergence | `context:navigateTree` | Tree mutation is rejected explicitly. |
| `switchSession` | safe divergence | `context:switchSession` | Session replacement is rejected explicitly. |
| `reload` | safe divergence | `context:reload` | Runtime reload is rejected explicitly. |
| `replacement.sendMessage` | safe divergence | `context:replacement.sendMessage` | Replacement-session messaging is rejected explicitly. |
| `replacement.sendUserMessage` | safe divergence | `context:replacement.sendUserMessage` | Replacement-session messaging is rejected explicitly. |

## Official example inventory

| Entry | Kind | Load fixture | Behavioral fixture |
| --- | --- | --- | --- |
| `auto-commit-on-exit.ts` | file | `example-load:auto-commit-on-exit-ts` | `example-registration:auto-commit-on-exit-ts` |
| `bash-spawn-hook.ts` | file | `example-load:bash-spawn-hook-ts` | `example-registration:bash-spawn-hook-ts` |
| `bookmark.ts` | file | `example-load:bookmark-ts` | `example-registration:bookmark-ts` |
| `border-status-editor.ts` | file | `example-load:border-status-editor-ts` | `example-registration:border-status-editor-ts` |
| `built-in-tool-renderer.ts` | file | `example-load:built-in-tool-renderer-ts` | `example-registration:built-in-tool-renderer-ts` |
| `claude-rules.ts` | file | `example-load:claude-rules-ts` | `example-registration:claude-rules-ts` |
| `commands.ts` | file | `example-load:commands-ts` | `example-registration:commands-ts` |
| `confirm-destructive.ts` | file | `example-load:confirm-destructive-ts` | `example-registration:confirm-destructive-ts` |
| `custom-compaction.ts` | file | `example-load:custom-compaction-ts` | `example-registration:custom-compaction-ts` |
| `custom-footer.ts` | file | `example-load:custom-footer-ts` | `example-registration:custom-footer-ts` |
| `custom-header.ts` | file | `example-load:custom-header-ts` | `example-registration:custom-header-ts` |
| `custom-provider-anthropic` | directory | `example-load:custom-provider-anthropic` | `example-registration:custom-provider-anthropic` |
| `custom-provider-gitlab-duo` | directory | `example-load:custom-provider-gitlab-duo` | `example-registration:custom-provider-gitlab-duo` |
| `dirty-repo-guard.ts` | file | `example-load:dirty-repo-guard-ts` | `example-registration:dirty-repo-guard-ts` |
| `doom-overlay` | directory | `example-load:doom-overlay` | `example-registration:doom-overlay` |
| `dynamic-resources` | directory | `example-load:dynamic-resources` | `example-registration:dynamic-resources` |
| `dynamic-tools.ts` | file | `example-load:dynamic-tools-ts` | `example-registration:dynamic-tools-ts` |
| `entry-renderer.ts` | file | `example-load:entry-renderer-ts` | `example-registration:entry-renderer-ts` |
| `event-bus.ts` | file | `example-load:event-bus-ts` | `example-registration:event-bus-ts` |
| `file-trigger.ts` | file | `example-load:file-trigger-ts` | `example-registration:file-trigger-ts` |
| `git-checkpoint.ts` | file | `example-load:git-checkpoint-ts` | `example-registration:git-checkpoint-ts` |
| `git-merge-and-resolve.ts` | file | `example-load:git-merge-and-resolve-ts` | `example-registration:git-merge-and-resolve-ts` |
| `github-issue-autocomplete.ts` | file | `example-load:github-issue-autocomplete-ts` | `example-registration:github-issue-autocomplete-ts` |
| `gondolin` | directory | `example-load:gondolin` | `example-registration:gondolin` |
| `handoff.ts` | file | `example-load:handoff-ts` | `example-registration:handoff-ts` |
| `hello.ts` | file | `example-load:hello-ts` | `example-registration:hello-ts` |
| `hidden-thinking-label.ts` | file | `example-load:hidden-thinking-label-ts` | `example-registration:hidden-thinking-label-ts` |
| `inline-bash.ts` | file | `example-load:inline-bash-ts` | `example-registration:inline-bash-ts` |
| `input-transform-streaming.ts` | file | `example-load:input-transform-streaming-ts` | `example-registration:input-transform-streaming-ts` |
| `input-transform.ts` | file | `example-load:input-transform-ts` | `example-registration:input-transform-ts` |
| `interactive-shell.ts` | file | `example-load:interactive-shell-ts` | `example-registration:interactive-shell-ts` |
| `kimi-deferred-tools.ts` | file | `example-load:kimi-deferred-tools-ts` | `example-registration:kimi-deferred-tools-ts` |
| `mac-system-theme.ts` | file | `example-load:mac-system-theme-ts` | `example-registration:mac-system-theme-ts` |
| `message-renderer.ts` | file | `example-load:message-renderer-ts` | `example-registration:message-renderer-ts` |
| `minimal-mode.ts` | file | `example-load:minimal-mode-ts` | `example-registration:minimal-mode-ts` |
| `modal-editor.ts` | file | `example-load:modal-editor-ts` | `example-registration:modal-editor-ts` |
| `model-status.ts` | file | `example-load:model-status-ts` | `example-registration:model-status-ts` |
| `notify.ts` | file | `example-load:notify-ts` | `example-registration:notify-ts` |
| `overlay-qa-tests.ts` | file | `example-load:overlay-qa-tests-ts` | `example-registration:overlay-qa-tests-ts` |
| `overlay-test.ts` | file | `example-load:overlay-test-ts` | `example-registration:overlay-test-ts` |
| `permission-gate.ts` | file | `example-load:permission-gate-ts` | `example-registration:permission-gate-ts` |
| `pirate.ts` | file | `example-load:pirate-ts` | `example-registration:pirate-ts` |
| `plan-mode` | directory | `example-load:plan-mode` | `plan-mode:full-journey` |
| `preset.ts` | file | `example-load:preset-ts` | `example-registration:preset-ts` |
| `project-trust.ts` | file | `example-load:project-trust-ts` | `example-registration:project-trust-ts` |
| `prompt-customizer.ts` | file | `example-load:prompt-customizer-ts` | `example-registration:prompt-customizer-ts` |
| `protected-paths.ts` | file | `example-load:protected-paths-ts` | `example-registration:protected-paths-ts` |
| `provider-payload.ts` | file | `example-load:provider-payload-ts` | `example-registration:provider-payload-ts` |
| `qna.ts` | file | `example-load:qna-ts` | `example-registration:qna-ts` |
| `question.ts` | file | `example-load:question-ts` | `example-registration:question-ts` |
| `questionnaire.ts` | file | `example-load:questionnaire-ts` | `example-registration:questionnaire-ts` |
| `rainbow-editor.ts` | file | `example-load:rainbow-editor-ts` | `example-registration:rainbow-editor-ts` |
| `reload-runtime.ts` | file | `example-load:reload-runtime-ts` | `example-registration:reload-runtime-ts` |
| `rpc-demo.ts` | file | `example-load:rpc-demo-ts` | `example-registration:rpc-demo-ts` |
| `sandbox` | directory | `example-load:sandbox` | `example-registration:sandbox` |
| `send-user-message.ts` | file | `example-load:send-user-message-ts` | `example-registration:send-user-message-ts` |
| `session-name.ts` | file | `example-load:session-name-ts` | `example-registration:session-name-ts` |
| `shutdown-command.ts` | file | `example-load:shutdown-command-ts` | `example-registration:shutdown-command-ts` |
| `snake.ts` | file | `example-load:snake-ts` | `example-registration:snake-ts` |
| `space-invaders.ts` | file | `example-load:space-invaders-ts` | `example-registration:space-invaders-ts` |
| `ssh.ts` | file | `example-load:ssh-ts` | `example-registration:ssh-ts` |
| `status-line.ts` | file | `example-load:status-line-ts` | `example-registration:status-line-ts` |
| `structured-output.ts` | file | `example-load:structured-output-ts` | `example-registration:structured-output-ts` |
| `subagent` | directory | `example-load:subagent` | `example-registration:subagent` |
| `summarize.ts` | file | `example-load:summarize-ts` | `example-registration:summarize-ts` |
| `system-prompt-header.ts` | file | `example-load:system-prompt-header-ts` | `example-registration:system-prompt-header-ts` |
| `tic-tac-toe.ts` | file | `example-load:tic-tac-toe-ts` | `example-registration:tic-tac-toe-ts` |
| `timed-confirm.ts` | file | `example-load:timed-confirm-ts` | `example-registration:timed-confirm-ts` |
| `titlebar-spinner.ts` | file | `example-load:titlebar-spinner-ts` | `example-registration:titlebar-spinner-ts` |
| `todo.ts` | file | `example-load:todo-ts` | `example-registration:todo-ts` |
| `tool-override.ts` | file | `example-load:tool-override-ts` | `example-registration:tool-override-ts` |
| `tools.ts` | file | `example-load:tools-ts` | `example-registration:tools-ts` |
| `trigger-compact.ts` | file | `example-load:trigger-compact-ts` | `example-registration:trigger-compact-ts` |
| `truncated-tool.ts` | file | `example-load:truncated-tool-ts` | `example-registration:truncated-tool-ts` |
| `widget-placement.ts` | file | `example-load:widget-placement-ts` | `example-registration:widget-placement-ts` |
| `with-deps` | directory | `example-load:with-deps` | `example-registration:with-deps` |
| `working-indicator.ts` | file | `example-load:working-indicator-ts` | `example-registration:working-indicator-ts` |
| `working-message-test.ts` | file | `example-load:working-message-test-ts` | `example-registration:working-message-test-ts` |

## Pi TUI audit

The Rust TUI is assessed as a semantic boundary rather than an arbitrary Pi component host. Every upstream test row remains visible below as an explicit safe divergence; this is intentionally narrower than a Pi TUI equivalence claim.

| Upstream test | Area | Status | Fixture |
| --- | --- | --- | --- |
| `test/autocomplete.test.ts` | autocomplete | safe divergence | `tui:test-autocomplete-test-ts` |
| `test/bug-regression-isimageline-startswith-bug.test.ts` | terminal-image | safe divergence | `tui:test-bug-regression-isimageline-startswith-bug-test-ts` |
| `test/editor-history-keybindings.test.ts` | editor | safe divergence | `tui:test-editor-history-keybindings-test-ts` |
| `test/editor.test.ts` | editor | safe divergence | `tui:test-editor-test-ts` |
| `test/fuzzy.test.ts` | autocomplete | safe divergence | `tui:test-fuzzy-test-ts` |
| `test/input.test.ts` | input | safe divergence | `tui:test-input-test-ts` |
| `test/keybindings.test.ts` | keybindings | safe divergence | `tui:test-keybindings-test-ts` |
| `test/keys.test.ts` | keys | safe divergence | `tui:test-keys-test-ts` |
| `test/latex.test.ts` | markdown | safe divergence | `tui:test-latex-test-ts` |
| `test/layout.test.ts` | layout | safe divergence | `tui:test-layout-test-ts` |
| `test/markdown.test.ts` | markdown | safe divergence | `tui:test-markdown-test-ts` |
| `test/native-module-path.test.ts` | native-module | safe divergence | `tui:test-native-module-path-test-ts` |
| `test/overlay-non-capturing.test.ts` | overlay | safe divergence | `tui:test-overlay-non-capturing-test-ts` |
| `test/overlay-options.test.ts` | overlay | safe divergence | `tui:test-overlay-options-test-ts` |
| `test/overlay-short-content.test.ts` | overlay | safe divergence | `tui:test-overlay-short-content-test-ts` |
| `test/regression-overlay-cjk-boundary.test.ts` | overlay | safe divergence | `tui:test-regression-overlay-cjk-boundary-test-ts` |
| `test/regression-regional-indicator-width.test.ts` | width | safe divergence | `tui:test-regression-regional-indicator-width-test-ts` |
| `test/select-list.test.ts` | widgets | safe divergence | `tui:test-select-list-test-ts` |
| `test/settings-list.test.ts` | widgets | safe divergence | `tui:test-settings-list-test-ts` |
| `test/stdin-buffer.test.ts` | stdin-buffer | safe divergence | `tui:test-stdin-buffer-test-ts` |
| `test/tab-width.test.ts` | width | safe divergence | `tui:test-tab-width-test-ts` |
| `test/terminal-colors.test.ts` | terminal | safe divergence | `tui:test-terminal-colors-test-ts` |
| `test/terminal-image.test.ts` | terminal-image | safe divergence | `tui:test-terminal-image-test-ts` |
| `test/terminal.test.ts` | terminal | safe divergence | `tui:test-terminal-test-ts` |
| `test/truncate-to-width.test.ts` | width | safe divergence | `tui:test-truncate-to-width-test-ts` |
| `test/truncated-text.test.ts` | widgets | safe divergence | `tui:test-truncated-text-test-ts` |
| `test/tui-alt-screen.test.ts` | tui | safe divergence | `tui:test-tui-alt-screen-test-ts` |
| `test/tui-cell-size-input.test.ts` | tui | safe divergence | `tui:test-tui-cell-size-input-test-ts` |
| `test/tui-overlay-style-leak.test.ts` | overlay | safe divergence | `tui:test-tui-overlay-style-leak-test-ts` |
| `test/tui-render.test.ts` | tui | safe divergence | `tui:test-tui-render-test-ts` |
| `test/tui-shrink.test.ts` | tui | safe divergence | `tui:test-tui-shrink-test-ts` |
| `test/word-navigation.test.ts` | editor | safe divergence | `tui:test-word-navigation-test-ts` |
| `test/wrap-ansi.test.ts` | width | safe divergence | `tui:test-wrap-ansi-test-ts` |

## Plan-mode journeys and deferred bridge remainder

The plan-mode fixture keeps the six upstream journeys visible while separating
existing bounded behavior from the deferred bridge remainder. Its hermetic fake
Pi checks explicit rejection; it is not a substitution for the
integrity-verified unchanged-source full gate.

| Journey | Fixture | Assertion |
| --- | --- | --- |
| plan-toggle-and-policy | `plan-mode:toggle-policy` | Deferred: active-tool policy and widget mutation reject explicitly; status contributions remain available. |
| plan-interception | `plan-mode:interception` | Supported: bounded tool-call interception routes through the declared mutation hook. |
| plan-persistence-resume | `plan-mode:persistence-resume` | Supported: durable extension entries and entry labels are stored behind `session_entries`; session snapshots and session/tree replacement stay host-owned and reject explicitly. |
| plan-dialogs-and-widgets | `plan-mode:dialogs-widgets` | Deferred: select/input dialogs remain available, while editor and widget transport reject explicitly. |
| plan-messaging | `plan-mode:messaging` | Supported: session naming and user-message injection behind `session_entries`/`message_injection`; assistant/system `sendMessage` and replacement-session delivery reject explicitly. |
| plan-commands-flags-shortcuts | `plan-mode:commands-flags-shortcuts` | Supported: native command catalog plus runtime shortcut registration and `shortcut/trigger` dispatch behind `shortcuts`; flag projection stays deferred and rejects explicitly. |

The current bridge does not consume `host.pi_compat` or emit `pi/*` child methods.
CLI/flag projection, session/tree replacement, assistant/system message
injection, widget/editor transport, and the remaining Pi
bridge surfaces are deliberately deferred. Outside the explicit API `0.3`
provider mode, provider/OAuth registration remains an explicit safe divergence;
even in that mode callbacks, credential payloads, endpoint/header/transport
authority, arbitrary component rendering, terminal ownership, model mutation,
session-tree mutation, and shutdown remain explicit safe divergences. The bridge
does not proxy credentials or grant new shell/network authority.

## Integrity-verified unchanged-source full gate

Run only with locally supplied artifacts; the command performs no download. It verifies both npm SRI values, matches the selected coding-agent root and the Pi TUI root Node resolves from it against their verified tarballs, requires a clean checkout at `b79e4cc834970cca69daebffab7df1da7d1e52c4`, fingerprints every source immediately before loading, clears credentials through an allowlisted environment and fresh `HOME`, and uses `unshare --net`.

```sh
python3 extensions/octet-pi-compat/conformance.py --full --network-isolated \
  --coding-agent-tarball /local/pi-coding-agent-0.84.4.tgz \
  --tui-tarball /local/pi-tui-0.84.4.tgz \
  --pi-package /local/unpacked/pi-coding-agent \
  --source-root /local/pi-source-at-b79e4cc
```

The full gate initializes all 78 unchanged sources through Pi’s public loader. It does not turn extension initialization into permission to use credentials or the network; Linux user/network namespace support is a prerequisite.

The bridge uses Pi's public resource loader with only the explicitly pinned paths
and in-memory settings. Workspace/global extension discovery and configured Pi
packages are excluded before any extension is imported. Explicit directories
retain Pi's `index.ts`/`index.js` fallback when no `pi.extensions` entries are
declared, even if unrelated prompts, skills, or themes are present. The whole
selected directory remains pinned. Post-load source and runtime integrity checks
remain in place; they are not a substitute for that pre-load selection boundary.

Prepare each example's locked dependencies separately before the isolated gate;
the bridge and gate do not install missing dependencies. A load failure is not
permission to rewrite an example or remove its aggregate-count check.

## Aggregate publication and API 0.3 evidence seam

A Pi aggregate is published only from a canonical, inert plan. The plan and its
published aggregate lock pin source order, each bounded source fingerprint,
nearby dependency-lock fingerprints, the exact Pi runtime path and package
integrity, bridge/Pi/octet versions, and the explicit-enable/explicit-trust mode.
Preflight repeats those checks without importing source; publish repeats
preflight immediately before writing the generated package. The bridge validates
the aggregate/link identity before invoking Pi's loader and rechecks runtime
integrity after loading, so a source or runtime changed between review and start
fails closed rather than becoming a best-effort load.

Published packages include `pi-runtime-evidence.json`, canonicalized with octet's
API 0.3 metadata helper. It records static aggregate selection, integrity, and
trust-binding evidence for a future runtime manager. The sidecar itself is not
a Pi API 0.3 process protocol and does not provide lazy
activation/workspace/reload semantics. API `0.2` links retain API `0.2` live
bridge coverage. An aggregate explicitly installed with `--api-version 0.3`
instead launches the constrained provider contract described above; it does not
enable lifecycle, migration, session, or dynamic-command support.

`octet pi rollback NAME` is intentionally non-destructive: it only moves a
validated generated package out of discovery into a rollback directory. It does
not delete reviewed Pi sources, rewrite an arbitrary extension, or grant/revoke
octet extension trust.

## Release policy

A broader Pi-equivalence release needs a separately approved decision for every
safe divergence, real-runtime evidence from the full gate, generated API and
API 0.1/0.2 regression checks, restart/trust/source-change/sanitized-environment
coverage, and a deliberate decision on the 33 TUI audit rows and provider/OAuth
boundary. The constrained API `0.3` provider bridge additionally needs exact
pinned-runtime and real-host evidence for declaration/catalog acknowledgement,
authorization status/refresh, semantic request hooks, canonical streaming,
cancellation, replacement/unregister cleanup, and rejection of credential,
endpoint, header, transport, callback, and OAuth payload authority. The deferred
shortcut, CLI/flag, session-control, and Pi bridge remainder lanes require their
own bounded contracts and tests before their statuses can change. Until then this
profile remains honestly named dogfood conformance.
