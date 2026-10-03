---
stage: engineering-design
status: needs-technical-decision
updated: 2026-10-02
---

# Pi extension power parity over subprocess JSON-RPC

**Design proposal, not a shipping compatibility claim.** Requested scope: continue
the remote-component design so the user's eight-package Pi corpus can work,
without making Node, Pi, or a second terminal renderer part of octet's core.
No runtime implementation, package execution, installation, or release is
introduced by this document.

The product outcome and constraints come from the user's current request and
the earlier power-parity direction. This proposal replaces conflicting *design
recommendations* in the historical wire plan (not bundled here)
for this scope; it does not retroactively change released contracts or the
[current product documentation](../pi-migration.md).

## 1. Committed outcome, requirements, and non-goals

**Outcome:** an explicitly enabled, optional Pi compatibility package loads
reviewed Pi extensions without source edits. Their callbacks and component
objects live in its subprocess. Rust exposes equivalent generic powers and
continues owning the agent, persistence, terminal, effects, and cleanup.

Requirements derived from that outcome:

| ID | Requirement |
| --- | --- |
| R1 | Qualify the five compact packages and three coverage additions by real behavioral scenarios, not API-name counts alone. Record versions, source/lock identities, runtime profile, configuration, and unsupported paths. |
| R2 | Keep the native extension path language-neutral and dependency-light. Node/JS/Pi dependencies exist only in the optional compatibility package. Disabled compatibility costs no JS startup or UI polling. |
| R3 | Support tools, commands, shortcuts, flags, persisted custom entries, custom messages, and their real data/registration semantics. |
| R4 | Preserve synchronous getters, read-after-write, callback ordering, cancellation, and contexts retained after the originating request returns. |
| R5 | Project headers, footers, keyed widgets, custom editors, overlays, fullscreen components, tool/message/entry renderers, indicators, autocomplete, and themes through the existing Rust renderer. |
| R6 | Preserve both observation and interception: input/context/tool changes, resource discovery, provider payload/header hooks, and response observations. |
| R7 | Expose session/model/reasoning control, custom compaction and branch-summary replacement/cancellation, replacement-session callbacks, and coordinated shutdown. |
| R8 | Preserve background completion, wake-up, steering/follow-up/next-turn delivery and session-scoped lifetime. |
| R9 | Support extension providers with faithful streaming, cancellation, registration/unregistration, and provider-local runtime dependencies. |
| R10 | Account for runtime imports, Pi CLI/SDK child creation, session-file assumptions, and version-specific component patches outside the reported 85-name metric. Do not silently run a second Pi agent and call it an octet child. |
| R11 | Preserve host ownership, stale-generation/owner fencing, UI rescue/restoration, bounded transport, and existing effect policy. |
| R12 | Require synthetic-host tests plus actual Rust-host/PTY qualification before claiming a surface or package works. |

Non-goals:

- Embedding V8, adding Node/npm to default installation/build/startup, or exposing
  Rust memory/locks/objects to extensions.
- A second terminal renderer, polling each component on every paint, or RPC per
  pixel. Extension-local Pi widgets may compute lines; they never start Pi's
  terminal writer.
- Executing packages during ordinary discovery, migration dry runs, or static
  help. Install/probe/qualification execution is separate and explicit.
- Implementing a general workflow graph/scheduler in the kernel. Extensions
  retain orchestration; generic child sessions need lifecycle and limits.
- Claiming every Pi version, arbitrary internal monkey-patching, arbitrary
  resource sizes, or pixel-identical default Pi chrome is compatible.
- Changing API `0.1`/`0.2`/canonical `0.3`, auto-retagging manifests, restoring
  removed production bridge code by assumption, or publishing a release.

## 2. Corpus and version evidence

All eight packages were found under `$PI_CORPUS`, here
`~/.pi/agent/npm/node_modules`. Their manifests and selected implementation
paths were inspected without importing or executing package code.

| Package | Installed version | Required behavioral gate |
| --- | --- | --- |
| `pi-agent-extensions` | `0.5.4` | Persist/replay state; handoff/switch/tree operations; tool/command/flag/shortcut registration; interactive custom screens and footer/editor replacement. |
| `pi-subagents` | `0.59.0` | Structured partial/final results, cancellation, async completion and steering; child CLI/session compatibility. |
| `pi-background-tasks` | `2.4.2` | A job completes after its tool returned, emits its custom message once, wakes the correct session, and cleans up on shutdown; provider attribution/header/payload behavior. |
| `pi-zentui` | `0.21.0` | Custom editor typing/submission/restoration, factory identity, footer updates, custom entries, working indicators and supported component style patches. |
| `@llblab/pi-telegram` | `0.39.5` | Resource discovery, model/thinking control, callback-style compaction, and ordered full tool execution observations. |
| `pi-claude-bridge` | `0.7.0` | Custom provider/tool round-trip plus compaction/tree-summary replacement, cancellation, and provider teardown. |
| `pi-langfuse` | `1.5.15` | Actual request and HTTP-response observations in order, with no response-header logging leak. |
| `@tintinweb/pi-subagents` | `0.19.0` | Autocomplete wrapper/delegation/application; imported session SDK behavior and bounded nested children. |

The reported **82/85 and 85/85 are API-name coverage**, not qualification results.
This investigation has not independently reproduced that set-cover calculation.
The existing bounded AST inventory in `crates/octet-coding-agent/src/migrate.rs`
should generate the pinned callsite manifest; do not replace it with a regex
count or treat inventory classification as executable compatibility.

### Source profile

- The local Pi checkout is now **1.0.0** at
  `7fbbd5f4a1d982bb02d63472dde0774fa639f99b`, not the versions in old plans.
- Local tag **v0.84.4** resolves to
  `b79e4cc834970cca69daebffab7df1da7d1e52c4` and was inspected via `git show`.
- `pi-background-tasks` declares Pi peer ranges `^0.81.1 || ^0.82.1 || ^0.83.0 ||
  ^0.84.0`, excluding 1.0.0. Other selected coding-agent peer ranges admit
  0.84.4. Therefore **0.84.4 is the first candidate qualification profile**,
  not proof all package features work on it. Resolve transitive dependencies
  and runtime exports before freezing that profile. Add 1.0.0 as a separately
  tested profile; do not silently widen a version range.

### Important callsite evidence

Paths below are relative to `$PI_CORPUS`; Pi references marked `v0.84.4:` refer
to the immutable Git source, not the current checkout's different line numbers.

- `pi-agent-extensions/extensions/handoff/index.ts:112,202` uses session context
  projection and `newSession`; `extensions/sessions/index.ts:856` switches a
  selected session path. `extensions/control/index.ts:961` registers a flag.
- `pi-zentui/extensions/zentui/index.ts:276,652,780` compares/restores actual
  editor factory objects. `ui.ts:998` forwards `onSubmit`. `footer.ts:193`
  receives `tui`, theme, and footer data, not just a text string.
- `pi-zentui/extensions/zentui/accent-rail-layout-patch.ts:210,308,336,426`
  discovers and patches JS layout prototype methods. Public method counts
  miss this compatibility dependency.
- `pi-subagents/src/runs/foreground/execution.ts:350,587` launches a child in
  `--mode json -p`; `src/runs/shared/pi-spawn.ts:27` resolves Pi's package root.
- `pi-background-tasks/src/extension.ts:219,473,486` delegates messages and
  lifetime events; `src/core/registry.ts:2327` requests follow-up/trigger-turn
  delivery after task completion. `src/core/anthropic-attribution.ts:1909,1980`
  emits/handles a shared claim probe. `src/fusion-child-extension.ts:628,633`
  installs header and payload hooks.
- `@llblab/pi-telegram/lib/skills.ts:18` returns resource discovery;
  `lib/pi.ts:262` calls callback-style `ctx.compact`; `lib/lifecycle.ts:613–621`
  handles tool start/update/end.
- `pi-claude-bridge/src/index.ts:1972,2029,2070` intercepts compaction/tree
  operations and registers `streamSimple`; failed takeovers explicitly cancel
  rather than falling back to unrelated native summaries.
- `pi-langfuse/index.ts:124,128` observes request/response boundaries.
- `@tintinweb/pi-subagents/src/ui/agent-mention.ts:133,171,175` wraps
  `getSuggestions`, delegates `applyCompletion`, and preserves file-completion
  triggering. `src/agent-runner.ts:13,299,1008` imports `createAgentSession`,
  replaces `session.agent.beforeToolCall`, and creates SDK child sessions.
  `src/nested-tools.ts:45` defaults nesting to two levels.

These are inspected paths, **not completed end-to-end tests**.

## 3. Current-state and reuse map

| Existing component | Evidence / current status | Reuse / change |
| --- | --- | --- |
| Executable process transport | `extension_process.rs:5403` defines `ExtensionRuntimeConfig` with bounded requests/writer, cancellation, supervisor, optional provider/session/bus bindings. | Reuse duplex reader/writer, tombstones, generation management, and feature negotiation. No actor/thread-per-package redesign. |
| Pi execution | `docs/pi-migration.md:13` says the production bridge was removed. `extensions/octet-pi-compat` is absent. | The historical ledger/path is not a live implementation baseline. |
| Optional runner POC | `examples/extensions/pi-compat-poc/runner.mjs:66`, README:88–119: limited text tools/commands and hooks, `hasUI: false`, no live state mirror. | Reuse loader/framing lessons; graduate only behind qualification. Current POC is not the production SDK. |
| Components | `tui/extension_components.rs:141,205` holds slot/line scaffolding and queued render requests. Searches found no live use of that surface in the host loop/renderer. | Reuse useful slot/cache structures after lifecycle/validation changes; do not interpret comments/tests as live wiring. |
| Fullscreen proposal | `extension_remote_ui.rs:33,114` has DTOs/SGR validation; `lib.rs:85,210` exports it. No `ui/frame` or remote-wake dispatch was found in the process/interactive paths inspected. | Complete the root slice, then use the same cache/compositor for general components. Do not claim Doom is playable from these types. |
| Renderer | `sexy-tui-rs` native `Component`/line-differential renderer; [TUI contract](octet-tui.md). | Remains sole terminal writer. Cached remote components fit its existing line contract. |
| Semantic UI | `extensions.rs:5311` applies bounded semantic contributions; `:5617` observes terminal input. | Preserve these cheap APIs; observation does not yet provide consuming focused Pi input. |
| Session control | `extension_process.rs:5345` has an epoch-fenced lifecycle queue, configured only for the canonical service; `extensions.rs:3036` exposes its consumer. | Reuse transaction/idle-boundary mechanics. API 0.4 needs an explicit feature binding and richer entry-addressed operations. |
| Model/control queues | `agent.rs:1399,1498` has `RunControl::steer/follow_up`. | Extend typed custom-message delivery and idle wake/receipts, not a second agent loop. |
| Providers | `ExtensionRuntimeConfig.provider_registry` at `extension_process.rs:5428`; coding `ExtensionProviderRuntime` at `extensions.rs:1558`. | Reuse registry, lifecycle-owned catalog, bounded streams and acceptance semantics. Offer a distinct 0.4 binding; canonical 0.3 is unchanged. |
| HTTP hooks | `octet-ai/src/runtime.rs:83,96,114,128` has synchronous header/payload/response hooks. Header transforms run before auth and reserve routing/auth names. | Add cancellable async mediation at real encode/HTTP boundaries. Blocking RPC inside these synchronous hooks is not the design. |
| Event bus | [event-bus.md](../extensions/event-bus.md): canonical 0.3, scalar-only, namespaced, session-isolated. | Preserve it. It is not Pi's arbitrary in-process `events` object. |
| Themes | [themes.md](../themes.md): native typed TOML discovery already works; no manifest theme root. | Add adapter palette/discovery projection. Do not claim themes are disabled or use a theme file to implement UI. |
| Compaction | `extension.rs:879` installs a strategy; current 0.4 process `compaction_strategy` supplies bounded image frames. | That hook is not Pi's textual compaction replacement or tree-summary veto. Add distinct typed interception at those transaction boundaries. |

All code evidence is from a dirty working tree. No existing code or unrelated
changes are replaced by this design.

## 4. Architecture and authority

```text
 native process extension                    optional Pi compatibility process
 (Rust/Python/other language)                 Node + loader + profile-pinned exports
             |                               Pi factories, widgets, callbacks,
             |                               local object/event/SDK facades
             +------------- duplex JSON-RPC -------------+
                                                          |
                     Rust ExtensionProcess transport / bounded admission
                            /              |             \
             generic control/registries   hook pipeline   component mailbox
                         |                  |                    |
             Agent / Session / RunControl  AI boundary    coding-host compositor
                         |                  |                    |
                    durable state       provider stream       sexy-tui-rs
                                                              terminal writer
```

No Pi-dependent code enters `octet-agent`, `octet-ai`, or `sexy-tui-rs`.
Generic host services are built only when required by behavior. The adapter
translates Pi names/shapes; it never owns host approval or persistence authority.

### Compatibility process topology

Use **one opt-in Pi compatibility domain per host execution context**, loading
selected Pi modules in deterministic Pi load order. Independent native
extensions keep their separate processes. Pi callbacks, shared symbols, factory
identity, and synchronous cross-extension event mutation remain local to the
Pi domain. Do not create one Node process per Pi package and then pretend JSON
fan-out preserves arbitrary JS object references.

This enlarges the crash domain to the selected Pi set, just as Pi's own loader
does. Host generation cleanup removes the whole domain's registrations/views.
Package provenance is retained for diagnostics and collision behavior; process
separation is not an OS sandbox.

## 5. Contracts and state

**All new names below are proposed**, except explicitly identified existing
methods. New bindings use optional API `0.4` features; feature offers require an
actual frontend/service consumer. The API number alone grants nothing. Existing
wires and generated canonical schema/models are not relaxed or relabeled.

### 5.1 Retained contexts, snapshots, and synchronous methods

Proposed feature: `retained_context_v1`.

A session-service request uses a strict envelope:

```text
{ resource_owner: {session_id, extension_instance_id, process_generation},
  parent_request_id?: integer, operation_id?: string, ...typed method params }
```

An active parent supplies its authoritative owner. Calls after it settles use a
host-issued retained binding, never a dead parent ID or invented owner. Request,
run, session, and process lifetimes are explicit; owning request completion is
not extension-session shutdown. Session/process retirement revokes its handles.
Resource authority and the Pi-visible session UUID/path are different fields.

The host sends revisioned `context/snapshot` and ordered `context/changed`
projections only to subscribers. Include real mode/dialog/component
availability, current model/reasoning/catalog, session entries/tree/head,
commands/tools, composer/editor revision, queue/idle state, usage, theme palette,
keybindings and footer data as needed. Cache full session projections in the
optional process with incremental updates and revision-anchored paged hydration;
do not clone history per paint or truncate `getEntries()` while claiming it is
complete. A lost sequence requires resync before dependent callbacks run.

Pi's synchronous methods cannot all become async RPC:

- Pure getters (`getEditorText`, `getEntries`, `getThinkingLevel`, footer data)
  read the local coherent mirror. Host state changes are applied before the
  callback whose event announces them.
- Editor factory getters return the actual locally registered function. Identity
  comparison/restoration in zentui must work.
- Synchronous void setters validate and update local state, emit ordered effects,
  and preserve read-after-write. Drain their effect barrier before later host
  hooks, submissions, or acknowledgements depend on them. Async timer-originated
  effects receive visible failure callbacks/diagnostics rather than being lost.
- A synchronous method returning a **host decision**, especially `setTheme`,
  must not optimistically fabricate success. Recommendation: a small adapter-only
  transport worker handles a bounded synchronous *leaf* request while Rust's
  independent control consumer remains live. Only this class may use that lane;
  no painting, approval, provider invocation, or recursive extension callback
  may depend on it. A mirror-only alternative is acceptable only if conformance
  proves equivalent return/failure behavior. This requires a deadlock fixture
  before graduation.

Long-lived handlers receive fresh host-driven mirror updates between user turns.
Operation cancellation is reconstructed as local `AbortSignal`s keyed by
operation IDs; it is not serialized as a JS object. Ambient Pi API routing and
captured `ctx` routing must be tested separately against the pinned runner.
Never silently retarget a revoked owner or replay an old side effect in a new
binding.

### 5.2 Registration, exports, flags, and resources

Reuse tool/command/shortcut/dynamic-catalog machinery; add only missing data and
consumer bindings. Preserve tool schemas, full result `details`/content parts,
renderer options/state, argument completions, raw command arguments, built-in
override behavior, and active/all-tool ordering where the profile uses them.
An override is a revision-pinned implementation binding, not permission to
bypass policy or keep an old approval after arguments/implementation changed.

Runtime exports are part of qualification: `CustomEditor`, component classes,
`getAgentDir`, schemas, model helpers, and SDK imports are not satisfied by a
`defineTool` shim. Pin/alias the observed package names and verify type-erased
versus actual runtime imports. Dependencies belong to the adapter/package.

Keep ordinary discovery static. An explicitly authorized install/refresh probe
may execute the factory to record registration metadata and generate a native
manifest, including Pi boolean/string flags. Startup verifies the source/profile
identity and authoritative initialized catalog; changed conditional registrations
must fail visibly or undergo a new authorized probe. Do not run a hidden Node
probe on every `--help`. Dynamic flags that cannot be represented by that phase
remain a qualification issue, not silently default-only `getFlag` support.

Resource-discovery callbacks are ordered requests whose results join the host's
normal trusted resource resolver. They do not make arbitrary discovered code
trusted/enabled. Skills/prompts/themes are actual resources, not notifications
claiming discovery happened.

### 5.3 Components: one cache and one compositor

Proposed feature: `ui_components_v1`; richer editor/autocomplete bindings are
separately negotiated. Finish the existing fullscreen `remote_ui` slice first.
Its open/frame/close convenience messages and general mounts share one backend,
not two competing component managers.

| Proposed message | Direction | Meaning |
| --- | --- | --- |
| `component/mount` | extension request | Bind a local component ID to a header, footer, keyed above/below-editor widget, editor, overlay, fullscreen, or addressed tool/message/entry region. Return admitted handle/layout revision. |
| `component/layout` | host notification | Actual width/height constraints, origin, layout revision, theme revision, terminal capabilities and focus. This triggers extension rendering outside the host paint path. |
| `component/frame` | extension notification | Complete latest frame: owner/handle, layout revision, frame revision, styled lines, optional typed cursor. Never imperative terminal code. |
| `component/event` | host notification | Sequenced key/paste/mouse/focus events for the current focused handle; raw terminal packet where the Pi profile needs it. |
| `component/unmount` | extension request | Remove the addressed binding and restore the ordinary projection. |
| `component/disposed` | host notification | Host lifetime ended; local `dispose()` is best-effort and does not block restoration. |

The host paint path reads an immutable cached frame through native `Component`.
The extension runs `render(width)` locally after invalidation/layout/input.
`tui.requestRender()` schedules that local work and publishes a new frame; it
never means synchronous Rust paint → extension render RPC.

State key: `(instance, generation, session binding, mount handle)`. Mount handles
change on replacement, even if a local component ID is reused. Layout revisions
prevent an old A→B→A size/theme/focus response from winning. Latest-wins applies
**per mount**, not globally (a game cannot overwrite the footer's mailbox).

Slot policy must reproduce the pinned Pi loader's setter/clear semantics and
ordered widget replacement. Header/footer/editor are real replacement slots,
not extra lines in the status list. Keep host defaults underneath; retirement
must not resurrect a disposed factory. Inspect Pi's collision rules before
freezing them—do not invent hidden priority behavior. Multiple visible overlays
need ordered handles, dynamic dimensions/visibility, focus restoration and
`done(result)` resolution; a single fullscreen open is not all of `ui.custom`.

Reuse current per-frame bounds (256 lines, 16 KiB/line, 512 KiB text, 1 MiB JSON
message) initially; advertise negotiated live-mount and aggregate memory limits.
Do not let individually bounded mounts produce unbounded total cache memory.
Virtualize historical renderer regions while retaining semantic source and
required adapter renderer state. Validate UTF-8/cell widths and admitted SGR;
clip using existing width-aware code and reset style boundaries. Cursor markers
from Pi editors become typed cursor metadata, not unchecked control sequences.
Images/links, when a profile needs them, use typed host artifact/placement data;
reject raw Kitty/OSC controls rather than claiming printable lines cover them.

Factories receive adapter-local TUI/theme/keybinding/footer-data objects. Pi
widget logic and component trees remain JS objects in the optional process.
Neither `TUI.start()` nor its terminal writer is run. Generic visual overrides
can target host semantic regions; Pi-specific class/prototype adaptation stays
in the profile. Zentui's version-specific `VStack`/component prototype patches
require their own scenario tests: exposing `setFooter` does not prove those
styles affect Rust-native components.

### 5.4 Editors, input, and autocomplete are behavioral protocols

Proposed features: `ui_editor_v1`, `ui_autocomplete_v1`.

A custom editor is **not** an image of a text box. Seed it with the current
revisioned draft. Route input to it exclusively; its local `onSubmit`, value,
cursor, history, paste and application-action callbacks must be installed before
focus. It sends ordered editor-state revisions and submit/action events. The
host commits the matching text/attachment state through the normal composer
submission path; it never submits an older cached value because Enter arrived
before a text update. An acknowledgement establishes a draft recovery point.
On crash, restore that point and handle unacknowledged input explicitly—never
claim unseen edits were preserved or blindly submit/replay ambiguous input.

Input and submit are lossless/control traffic, **not** latest-wins frames.
`CustomEditor` application actions map to host commands through the same ordinary
control path. Host confirmations/secret input remain a separate priority owner.
Host rescue/close controls work without a responsive extension. Raw observation
must support actual consume/rewrite/unsubscribe behavior before normal dispatch.
Preserve raw packets at the single Rust input-owner boundary; the existing
`raw_key_data` reconstruction helper does not prove Kitty modifier/release or
Unicode fidelity. Do not add a second reader competing for tty bytes.

Autocomplete wraps a provider, not just a string list. Preserve
`getSuggestions(lines,cursorLine,cursorCol,options)`, the previous provider,
`applyCompletion`, and `shouldTriggerFileCompletion`. The adapter retains local
wrapper objects; async delegation to a host-backed base provider uses revisioned
queries. Cache pure completion application/trigger rules locally where exact;
otherwise use the bounded leaf lane, never a paint callback. Stale query results
cannot replace a newer draft's menu. Test multiline cursor edits, file completion,
agent mentions, cancellation, and chained providers.

Theme/keybinding/footer mirrors include real palette, color depth, model/usage,
git branch and extension statuses with subscriptions. Translate Pi theme JSON
in the optional adapter and send a validated palette/native semantic projection;
keep native TOML and compiled defaults working. A theme is styling data, not a
layout program or an execution grant.

### 5.5 Hooks: effects, not just names or return types

Proposed feature: `pipeline_hooks_v1`, with declared subscriptions and typed
operation IDs. Extend existing hook dispatch at actual boundaries; Pi payload
translation remains adapter-local.

| Behavior | Transport / commit rule |
| --- | --- |
| Pure observations | Ordered notifications or ordered observation batches. Start/end facts are never dropped; attach a sequence and current state revision. |
| Input/context/tool/compaction/tree interception | Awaited cancellable request. Return validated decisions and modified data. Revalidate changed tool arguments and policy before execution. |
| `before_provider_request` | Awaited request carrying the actual encoded payload; replace with the selected result, not a canonical high-level message approximation. |
| `before_provider_headers` | Awaited request with the mutable header value returned explicitly by the adapter after handlers finish, despite handlers returning `undefined`. Apply deletion (`null`) and insertion semantics. |
| `after_provider_response` | Real status/headers at the HTTP response boundary, before body consumption. Preserve the profile's callback/order behavior; do not invent this event at stream completion. |

In Pi v0.84.4 `types.ts:700–706` the header event mutates in place and ignores
return values; `runner.ts:1100–1128` awaits handlers. The old plan's rule
“events with Result types are requests, others are notifications” is therefore
incorrect. Similarly, tool-call argument mutations cannot be discarded because
a handler only returned a block decision.

The current AI hooks are synchronous and have different auth/header restrictions.
Add an **optional async hook interface at encode/send/response boundaries**, inert
when absent. Never block the tokio executor inside `PayloadHook` to wait for a
subprocess response. Keep signing/auth ordering and reserved-header protections
explicit. A supported Pi profile needs exact tests for the headers it actually
changes; unsupported auth/routing interception is a visible profile limitation,
not a silent stripped mutation. Sensitive hook data is private protocol data:
no raw headers, credentials, or payloads in normal logs/session/UI traces.

The adapter runs Pi handlers in pinned load/registration order and returns both
in-place mutations and explicit results as required. No-handler defaults,
handler exceptions, explicit cancellation, transport loss and deadline expiry
have distinct outcomes. Match the pinned runner's continue-on-handler-error
where applicable with diagnostics; do not convert a received explicit compaction
or tree veto into a native fallback.

Do not retrofit mutable arguments into the existing `ToolCallHook` contract:
`extension.rs:81–105` deliberately makes it a secondary deny/observation lane
**after effect admission**. A proposed transformation needs a distinct
pre-admission boundary; the final arguments and implementation revision must be
validated and broker-admitted together. Result transformation changes the
model/display projection, not the host's actual execution/effect facts. Apply
these rules consistently to serial, parallel-read, and restart-recovery paths;
see Appendix A.4 for the inspected boundaries and illustrative flow.

### 5.6 Providers: lossless streams and callback routing

Proposed feature: `provider_proxy_v1`, reusing the existing registry/stream
implementation through a 0.4 service binding.

Provider registration carries secret-free catalog/model information and a
handler handle. Base URLs, key interpolation, OAuth functions, `streamSimple`
and custom SDK/transport state live in the optional provider process where
possible. Host-resolved credentials, if needed, use the existing explicit broker
boundary; do not return invented keys or put secrets in model/catalog snapshots.
Unregister/reload removes the override and restores the correct native/catalog
binding. Stream identity includes provider registration revision/generation.

A host `provider/stream` request receives explicit acceptance. Subsequent events
have `(stream_id, sequence, kind, payload)`, full text/thinking/tool-call/usage
semantics, and exactly one terminal disposition. Preserve partial data and tool
result continuation across subsequent stream calls; pi-claude-bridge depends on
that state. Cancellation reaches the local signal and underlying SDK operation.

**Never latest-wins a provider stream.** Batches may contain ordered event arrays
or concatenate compatible adjacent deltas without discarding data. Bounded queue
pressure backpressures or terminalizes the stream visibly. A dead generation or
ambiguous accepted request is not automatically replayed.

Provider-local `onPayload`, header transformation and `onResponse` callbacks map
to the same operation's hook pipeline. Use callback/attempt identity to prevent
both missed hooks and double emission from the host and the adapter. An SDK
operation with no exposed HTTP response must not fabricate response headers.
Provider-specific orchestration stays in its extension; the host owns the
accepted model stream and durable accounting.

### 5.7 Sessions, compaction, delivery, and shared events

Proposed features: `session_control_v1`, `message_delivery_v1`.

Session new/fork/switch/navigate/compact/reload/shutdown/wait-for-idle use the
existing host transaction/idle driver, not direct mutation of `Agent` from a
transport task. Entry IDs, full tree/head and message projections are real,
versioned state. Before hooks can cancel/replace where Pi allows it. Validate
summary/compaction anchors against the prepared source revision before committing
one durable entry. Image-based `compaction_strategy` is a different feature.

Session replacement returns a freshly fenced context for local `setup`/
`withSession` callbacks. A command cannot continue sending into the dead parent
binding. Setup callbacks require a host-managed transaction/handle; they do not
receive Rust's mutable `SessionManager`. `ctx.compact` is non-awaiting and invokes
`onComplete/onError` on its operation's terminal notification, matching the
profile. `ctx.shutdown` requests coordinated host close, not abrupt process exit.

Custom messages remain custom messages, not invented assistant/system provider
turns. Persist their content/details/type and honor `display` through the remote
renderer. `appendEntry` is durable extension state, not model input. Preserve
text/media, template-expansion choice and `deliverAs`:

- `steer`: enqueue at the admitted active-run boundary;
- `followUp`: ordered subsequent-turn delivery;
- `nextTurn`: stage the custom message for the next turn without pretending to
  trigger one;
- `triggerTurn`: idle wake or queued activation under the correct session owner.

Receipt identity distinguishes accepted, persisted, delivered, cancelled and
terminal work. A background message after the tool returns must still work;
a stale parent ID must not be its authority. Idle wake-up is notification-driven.
No automatic cross-session dispatch or duplicate turn on retry/late completion.

Pi's shared event bus stays local to the compatibility domain, preserving
synchronous payload-object mutation and unsubscribe semantics. The attribution
claim probe is an actual example. Optional native-peer communication can add a
new explicitly negotiated JSON payload mode to the existing bus implementation;
it is not the scalar-only canonical 0.3 bus and cannot carry JS functions or
object identity. No workflow graph is needed in the host for local Pi events.

## 6. SDK/CLI child sessions: the material decision

API-name coverage misses this entire layer. `pi-subagents` resolves and launches
Pi's own CLI; tintin imports session/resource/settings classes, overrides an
agent hook, and defaults nesting to two levels. The existing depth-one,
owning-run-scoped `agent_sessions` service is not a transparent substitute.

| Option | Benefit | Cost / conflict |
| --- | --- | --- |
| Let the optional package use upstream Pi SDK/CLI children | Smallest route to exercising many unchanged paths; JS and Pi remain extension-local. | A second agent implementation owns those children. Octet cannot claim its child budgets, event coverage, policy inheritance, or accounting. |
| **Provide adapter-local SDK/CLI facades over octet-owned children** | Meets the host-owned Rust-agent direction and permits real zero-source-edit compatibility. | Requires child SDK/export/JSON-mode/session-file qualification and evolution beyond today's run-scoped/depth-one service. |
| Require native ports of the child packages | Least core scope. | Does not meet the requested unchanged-extension outcome; cannot be called full package compatibility. |

**Recommendation: the second option.** Do not silently fall back to upstream Pi.
The proposed optional facade preserves the observed `createAgentSession`,
`SessionManager`, resource/settings loader, event subscription, messages, prompt,
steer, abort, dispose, and registered child-tool callback behavior. A package-root
CLI entry accepts the observed Pi argv/JSON event contract and drives the same
host-owned child service; merely putting `octet` earlier on PATH will not fix
`import.meta.resolve` followed by `node /.../pi/dist/cli.js`.

This needs an approved, explicitly bounded **session-lifetime child binding** and
**nested-child policy** distinct from the current depth-one owning-run service.
Native descendants inherit host authority/budgets; extensions own schedules and
roles. Child hook/tool callbacks retain their own owner IDs and cancellation
paths. Accounting settles exactly once to the durable owning session, including
children that complete between parent turns. Process/session shutdown and
owner replacement must cancel/settle the right descendants.

Pi session-file consumers need an explicit adapter projection/file ABI or SDK
facade. Do not hand Pi parsers native octet JSONL and claim the formats match,
forge an inaccessible session path, silently import historical Pi sessions, or
write two independent authoritative transcripts.

**Open technical decision:** approve the session-lifetime/bounded-nesting
extension of the host child contract, versus allowing separately labeled
Pi-backed children. Until resolved and its SDK surface inventoried, the complete
eight-package design is not ready for implementation as a single parity wave.
The registry, context, UI, hooks and delivery slices are independently specified.

## 7. Concurrency, backpressure, failure, and security

Reuse a single physical JSON-RPC writer with logical traffic classes:

1. Cancellation, lifecycle, responses and restoration control have reserved
   admission and scheduling capacity.
2. Hook decisions, provider/tool event sequences, editor state/submissions,
   mutations and delivery receipts are ordered/lossless.
3. UI frames and explicitly snapshot-valued progress are replaceable latest
   state. Lossless delta updates are never put in this lane.

Bound bytes as well as message counts. A partially written JSON line cannot be
preempted; stalled pipes trigger existing transport deadlines and cleanup, not
an indefinitely blocked rescue. Release/key/submit events are not arbitrarily
dropped when render traffic fills a queue. Subscriber-specific queue failure is
inspectable; no stale partial state presented as current.

While awaiting a callback, the host reader and safe reverse-request/control
consumer stay live. Do not hold the shell mutex or mutable Agent/session borrow
across RPC. Operation-changing requests enqueue into the owning driver; requests
which would wait on their own in-flight hook must be refused/deferred explicitly
rather than deadlocking. Async long operations use operation lifetimes, not an
ordinary short request kept open forever.

| Path / failure | Handling | Visibility / verification |
| --- | --- | --- |
| Feature absent / headless frontend | Omit offer; explicit unsupported response. | Adapter reports actual `mode`/availability; headless fixtures. |
| Stale owner, generation, mount, layout, operation | Reject/drop before state commit; never rebind/replay. | Bounded disposition diagnostic; A→B→A and reload race tests. |
| Snapshot gap / resync failure | Suspend dependent callbacks, hydrate a coherent revision or fail them explicitly. | No empty fabricated getters; sequence-gap tests. |
| Invalid ANSI/oversized frame | Reject before terminal output; retain last safe frame or unmount on transport retirement. | Hostile-frame/SGR/cursor/image tests. |
| Component/editor crash | Unmount without waiting; restore native projection and acknowledged draft. | Actual-host PTY restoration tests and explicit unacknowledged-input disposition. |
| Hook exception / timeout / explicit veto | Profile-specific error policy with diagnostic; preserve received veto and host validation. | No silent generic success/default on every failure; compaction/tree/provider fixtures. |
| Accepted provider dies / queue overflow | One stream failure, no automatic replay; preserve usage uncertainty when applicable. | Sequence/final-event/cancellation/acceptance-race tests. |
| Background completion after switch/close | Revalidate target binding; park/refuse according to delivery contract, no current-owner guess. | Receipt visible; no wrong-session messages/wakes. |
| Probe/profile/dependency/registration mismatch | Refuse qualification/startup with exact reason. | No auto-install, exec-on-discovery, or fake flags/tools. |
| Child SDK/CLI shape not covered | Explicit unsupported path until child gate lands. | Separate package/path status; never label public API coverage complete-package success. |

Current enablement, trust, OS authority, approvals and source-integrity rules
remain. UI freedom is not approval authority. Secret-bearing hook/broker data
is never included in ordinary state mirrors, bus events, logs or durable UI
frames. Diagnostic payloads expose bounded IDs/revisions/statuses, not raw
provider headers or private session contents. Terminal write logs remain
explicitly sensitive opt-in evidence.

## 8. Performance and observability gates

Native startup/idle must retain the current path when no compatible process or
subscriber is enabled. No Node lookup, new perpetual timer, full-session clone,
or per-token hook round-trip on that path. Subscribe once to host state changes;
render only invalidated/visible components. Extension timers are process-local.

Measure before promising performance:

- Native no-extension startup/idle versus baseline.
- Enabled idle adapter CPU/RSS and wake/frame counts.
- Input echo, Ctrl+G dismissal, Ctrl+D close and resize latency while a component
  floods maximum admitted frames, hooks are held, or a provider is streaming.
- Doom's original 35 Hz target: actual frame age, bytes/second, dropped superseded
  frames, bounded cache/writer depth and responsiveness, not just timer ticks.
- Footers/custom editor/entry rendering on short, regular and wide terminals;
  no-change frames stay quiet and native transcript/history is not corrupted.

Use existing startup/held-provider PTY baselines (including 500 ms
input/cancellation budgets) as regression gates. New projection/control targets
must be recorded with fixture hardware/environment, not claimed from design.

`/extensions status` should expose offered/negotiated features, adapter profile,
package identities, current generation/binding, mounts/focus, mirror revision,
queue high-water marks, frame replacements, pending operations, and bounded last
failure. Provider/child acceptance and terminal facts remain separate from UI
paint/completion. Add opt-in timing counters, not a telemetry service.

## 9. Qualification and requirement traceability

| Requirements | Component / flow | Required evidence |
| --- | --- | --- |
| R1, R10, R12 | Pinned corpus/profile inventory | AST callsite/export/CLI/session-file manifest plus per-package behavior report; counts are not a pass. |
| R2 | Optional adapter/discovery | Native build/startup/idle without Node; discovery/help never import package code; dependency-failure fixture. |
| R3 | Registry/flags/resources | Unchanged registration/tool/command/shortcut calls, data/schema/render options, raw arguments, flag values, persisted/replayed entries and resource discovery. |
| R4, R8 | Retained mirror/control lane | Timer callback after request return; setter→getter; getter before announced event; async shutdown; snapshot gap; synchronous-leaf deadlock/timeout test. |
| R5, R11 | Components/editor/compositor | Actual Rust host PTY: footer replacement, widget placements, custom editor submission/restore, overlays and fullscreen Doom, resize, crash, focus and host prompts. |
| R5 | Autocomplete/theme/renderer facade | Tintin wrapper preserves base suggestions/application/file triggering; zentui factory identity and supported patches; message/entry/tool renderer expansion and replay. |
| R6, R9 | Hook/provider boundary | Loopback HTTP records actual payload/header insertion/deletion; response status/header event before stream; ordered provider events/tool round-trip and cancellation. |
| R7 | Session transaction/compaction/tree | New/fork/switch/navigation cancellation and replacement callbacks; valid anchors; replaced summaries; veto/failure causes no unwanted commit/fallback. |
| R8 | Message delivery | Idle wake after tool return, busy steer/follow-up/next-turn, display flag, no duplicate message/turn, owner switch and shutdown races. |
| R9, R11 | Stream admission / queues | Gap/duplicate/final/overflow/crash/late result tests; exactly-one terminal and no replay after ambiguous acceptance. |
| R10, R11 | Child SDK/CLI path | Observed imports/argv/event consumers unchanged; bounded nested/session-lifetime jobs, cancellation, tool hooks and exactly-once accounting, session-file projection. |

Test tiers: generic Rust validation/lifecycle unit tests; adapter synthetic-host
conformance for each method/callback; loopback providers and fake external SDK/
Telegram/Langfuse transports; **real octet** process/PTY fixtures with unchanged
package entrypoints. Do not contact Telegram, Langfuse, Claude services or paid
providers without separate authorization. Source/profile-specific component
patches and external addons receive separate qualification rows.

Statuses should distinguish `inventory-only`, `synthetic-conformant`,
`host-qualified`, `requires-adaptation`, and `unsupported`, scoped to a scenario
and profile. A package is not globally marked passing because its factory loaded.
The old removed bridge's ledger cannot be updated as if it were still present;
a new optional adapter may regenerate its own evidence-backed compatibility
report once it exists.

## 10. Ordered implementation slices and rollout

Implementation requires approval; this request is design-only.

1. **Freeze the corpus/profile and resolve the child decision.** Reuse the AST
   inventory; add runtime export, internal-patch, CLI/SDK, session-file and
   collision coverage. Keep independently completable slices separate.
2. **Transport/context vertical slice.** Reuse process supervision; add retained
   bindings, coherent mirror, cancellation/control scheduling and effect
   barriers. Qualify one unchanged text tool plus an after-return timer action
   against the real host, without a paid model. This is the prerequisite for UI
   and background/provider callback fidelity.
3. **Fullscreen vertical slice.** Complete the generic remote UI dispatch and
   Rust frontend projection; qualify the original native pi-doom adaptation,
   resize/input/crash restoration. No Pi runtime dependency for this example.
4. **Slots and chrome vertical slice.** Same mailbox backend; real header/footer,
   widgets, working/hidden-thinking/expanded state, title, theme/footer/keybinding
   mirrors. Qualify pi-agent-extensions footer and zentui footer/indicator.
5. **Editor, overlay, and rendering vertical slice.** Revisioned draft/submission,
   cursor/input ownership, local factories, message/entry/tool renderers and
   callback disposal. Test zentui and Pi custom screens in an actual PTY.
6. **Autocomplete vertical slice.** Wrapper chaining and host-base-provider
   delegation/application with stale-result fencing. Qualify tintin mentions
   without disabling normal file completion.
7. **Semantic hooks/provider vertical slice.** Extend AI encode/header/response
   boundaries asynchronously; bind provider registry/streams to 0.4. Qualify
   background attribution, langfuse response observation and claude-bridge
   provider continuation with fake/loopback transports.
8. **Session/delivery vertical slice.** Rich custom messages, idle wake and queue
   receipts; entry-addressed session transactions, compaction/tree replacement,
   replacement callbacks and coordinated close. Qualify handoff/background/
   Telegram scenarios without contacting external accounts.
9. **Child SDK/CLI vertical slice, conditional on decision.** Thin optional
   facades plus approved generic child contract. Qualify both subagent packages'
   actual child path, nesting, lifetimes, files, hooks, cancellation and accounting.
10. **Integrated eight-package qualification.** Run individually, then supported
    combinations in deterministic load order, testing actual collisions and
    cross-extension events. Update public docs/compatibility claims only after
    gates pass. Full eight-package completion remains blocked on any unqualified
    SDK/CLI/internal-patch path, even if all 85 public names are recognized.

Parallel lanes after shared contract freeze: adapter facade/fixtures versus
individual host service bindings, with exclusive path ownership. Do not let
multiple workers independently redesign `extension_process.rs`, `extensions.rs`
or the renderer. Integrate one vertical slice before enlarging the feature offer.

Rollout is opt-in, feature/profile pinned, and reversible: disable the adapter
or stop offering a new service; existing native extensions continue unchanged.
No bulk manifest migrations or legacy-version union. Candidate-first reload
retains the old process if negotiation fails, but accepted replacement retires
its handles and restores the host before a new generation's UI is mounted.
Durable entries introduced by a new feature require explicit versioned record
variants/readers; rollback must preserve or visibly reject them, never erase
user history. UI frames and local factory handles are ephemeral, not a session
schema migration.

## 11. Decision log and remaining approval

- **Chosen:** generic subprocess services, sole Rust terminal writer, cached
  asynchronous projections, optional Pi-local runtime/dependencies, real
  profile-pinned behavior tests.
- **Corrected:** no mandatory Node backend/actor-thread per extension; no all-API
  version union; no synchronous RPC during paint; no loss of provider deltas
  through per-frame coalescing; no Result-type-only hook classification; no
  fake synchronous success or dead-parent authority; no API-count parity claim.
- **Recommended, requires approval:** session-lifetime/bounded-nesting child
  sessions plus optional SDK/CLI facades, instead of silently using Pi's agent.
- **Evidence still required:** reproduce 82/85 and 85/85 from pinned inventory;
  complete export/dependency/conditional-registration/collision audit; qualify
  zentui internal patches, synchronous-decision lane and child file/SDK semantics.

This document is ready for design review. It is **not** approval to implement
all eight packages, a test report, or proof of current Pi compatibility.

## Appendix A. OMP as a boundary stress test, not added product scope

The user supplied `~/github/can1357/oh-my-pi` as a reference for what an extension
system should make possible **if someone wanted those features**, explicitly not
as a request to add LSP or OMP's batteries to octet. The inspected reference HEAD
is `3b003d878c1c5e260e23b34dac4ceb3c967ecc94`; `$OMP` below denotes that checkout.
This is a representative source/documentation investigation, not a complete OMP
audit or execution/compatibility report. No OMP code was imported or run.

**Principal finding:** substantial OMP behavior uses built-in integration points,
not merely its public extension API. Porting the feature names or exposing 85 Pi
methods would not, on its own, give an isolated extension those powers. Remote
components solve terminal projection; they do not solve native-operation
composition, fresh background results, or agent-stream control.

### A.1 LSP reveals the difference between a tool and deep integration

A basic extension can own a language-server subprocess, speak LSP, and return
hover/definition/diagnostics through ordinary registered tools. That needs no
LSP engine, server catalog, npm dependency, or diagnostic cache in Rust core.

OMP's **integrated** LSP does considerably more:

- `$OMP/packages/coding-agent/src/tools/write.ts:482,934–945` installs an LSP
  writethrough, receives the final formatted content, and builds the native
  write snapshot/result from those actual bytes. A post-tool notification that
  formats the file again would leave that bookkeeping describing the wrong
  write.
- `$OMP/packages/coding-agent/src/lsp/writethrough.ts:379–407,433–516` coordinates
  document overlays, formatting, byte commits, watched-file notifications,
  document versions and save notifications. During a pending write it prevents
  a concurrent query from reverting the server to older disk contents.
- `$OMP/packages/coding-agent/src/lsp/deferred-diagnostics.ts:16–54` cancels an
  older pending fetch, tags a mutation version, and supplies a delivery-time
  stale predicate. `sdk.ts:2253,2262–2267,4845` connects that to the session yield
  queue and a session-wide file mutation ledger. Session/generation fencing
  alone is insufficient: a result can belong to the right session but the wrong
  file contents.
- `$OMP/packages/coding-agent/src/lsp/client.ts:563–579` handles server-initiated
  `workspace/applyEdit`, outside the ordinary model-issued tool action.
  `lsp/edits.ts:187–203` backs up reference files and restores them if the final
  rename fails. This is an effect/partial-completion problem, not just JSON-RPC
  transport. It is not proof of general multi-file atomicity under concurrent
  writers.
- `$OMP/docs/tools/lsp.md:50–62` describes initialization/cancellation, document
  reconciliation, capability handling and optional broker sharing; those are
  language-service responsibilities that can remain extension-local.

Therefore the useful **generic pressure points**, if such integration were later
requested, are:

1. **Native delegation or an operation middleware seam.** Extensions can compose
   with native tools without reimplementing their file resolution, stale-content
   guards, bookkeeping and cancellation. OMP has same-tool `ctx.invokeTool`
   (`docs/extensions.md:578–598`; `extensibility/extensions/wrapper.ts:110`).
   Octet must keep its own effective-argument/policy validation; same-name
   delegation is not permission to reuse approval for different effects.
2. **Prepare/transform/commit boundaries for mutations.** A formatter can return
   a bounded change proposal against a specific source revision; the host
   validates and commits it, then reports the final bytes/revision. Observers
   see committed facts. Slow/broken analysis must not ambiguously prevent,
   duplicate, or misreport the original write.
3. **Conditional deferred delivery.** Background results need source-resource
   freshness checked at delivery, not just at enqueue. An adapter-local closure
   such as `isStale()` cannot cross the wire: represent its dependency as typed
   revision/precondition data or an explicitly mediated decision. Do not make
   the kernel understand diagnostic severities or language-server versions.
4. **Owned resource lifetime and changesets.** Servers/watchers can outlive a
   tool call but retire on owner/workspace/config replacement. Multi-file
   edit/create/delete/rename proposals need explicit validation, effect policy,
   conflict checks and partial/rollback receipts if exposed as host operations.
   Never blindly restore old contents over another writer. Shared server pools
   remain extension-owned with explicit leases; do not install an LSP singleton
   in the host merely to support sharing.

These are candidate seam requirements, **not newly negotiated RPC methods,
implemented support, or additions to the eight-package acceptance scope**.
An extension may perform OS-allowed file/process operations directly today;
host-mediated safeguards do not magically sandbox those operations.

### A.2 Other inspected examples exercise the same boundary

| OMP example / evidence | What can remain extension-local | Generic host power it would need for deep integration |
| --- | --- | --- |
| DAP debugger: `docs/tools/debug.md:127–139,294–305` | DAP transport/adapters, breakpoints/stack caches, bounded output and protocol reverse requests. | Session-owned lifetime, cancellation versus termination, effect admission for launch/attach/evaluation, and optional remote views/input. A debugger subprocess tree is not an agent-session tree. |
| Persistent eval: `docs/tools/eval.md:102–110,143–158,231–247` | Python/JS runtime, cells, package environment, local objects and MIME interpretation. | Reentrant typed tool invocation through normal policy, lossless output/artifacts, owner cleanup, and separate servicing of child-tool callbacks while a parent waits. Never a mandatory host JS runtime or execution on the paint thread. |
| Stream rules: `docs/ttsr-injection-lifecycle.md:102–147,254–268` | Regex/AST/rule matching, rule discovery and repeat state. | Incremental stream observation plus generation-fenced interrupt/injection/continuation and explicit disposition of partial output. A before-request hook or generic abort followed by a message is not automatically equivalent to discard-and-retry semantics. Per-token synchronous RPC would violate the performance direction. |
| Artifacts/virtual resources: `docs/blob-artifact-architecture.md:122–143,153–193` | Output capture logic, extraction/formatting, provider-specific resources. | Bounded structured output and stable owner-scoped artifact references. If native read/search tools must handle new schemes, a resolver/delegation seam is required; registering a differently named tool alone does not do that. Capture failure must not advertise a complete artifact. |
| Collaboration: `docs/collab.md:57–71,129–146,191–205` | Relay, encryption, network identities, client applications. | Coherent semantic snapshot/events, explicitly admitted input/control and session-transition fencing. This is not a license to capture terminal secrets or grant remote guests every extension/host effect. |
| Composer/chrome customization: `docs/extensions.md:932–1006` | Shape/widget logic, styling and local factories. | Real layout budgets, cursor/IME metadata, resize/focus, and cached Rust composition—not just adding strings to a status list. |

This also provides a useful negative check: OMP's current interactive
`extension-ui-controller.ts:162–164` makes `setFooter` and `setHeader` no-ops while
wiring `setEditorComponent`. Its own extension guide documents the distinction
at `docs/extensions.md:799–808`. A method existing in a type is not evidence of a
live behavior, even in the reference system.

### A.3 How much heavy lifting belongs in octet?

- **Ordinary domain tools:** relatively little new host surface; most work lives
  in the extension and its protocol/service implementation.
- **Deep integration with native tools/UI/background state:** substantial,
  reusable contract and lifecycle work—delegation, mutation revisions, retained
  ownership, effect ordering, conditional delivery and real component mounts.
  Not importing OMP's language-service implementation into core.
- **Changing how the agent runs or who controls it:** explicit generic engine/
  control changes are unavoidable for faithful semantics. Hooks cannot be a
  euphemism for handing out a mutable `Agent` or arbitrary terminal access.

There is no measured engineering estimate from this investigation. The amount
of *domain implementation* and the amount of *core plumbing* are separate; a
small core can expose powerful boundaries without making a production LSP
integration itself a one-hour exercise.

Use OMP's regression scenarios as future contract probes: slow diagnostics after
write return, stale file versions, native wrapper delegation, rename failure,
server-originated edits, blocked parent/reentrant child calls, and stream
interruption concurrent with session replacement. Inspection of those tests is
not a claim they were run. Only approved features should turn a probe into a new
implementation slice; LSP, DAP, eval, collaboration and OMP compatibility remain
outside the committed scope.

### A.4 Concrete octet reuse and missing host seams

The following map distinguishes a **missing binding** over an existing sufficient
primitive from a **new seam** whose behavior the host does not yet expose. The
source shorthand below (`extension.rs`, `extension_process.rs`, `agent.rs`,
`events.rs`, `secure_fs.rs`, and `tools/*`) is under `crates/octet-agent/src/`.
All observations describe inspected source, not runtime qualification; candidate
additions remain outside OMP product scope.

| Pressure point | Existing octet machinery | Missing boundary / smallest reusable direction |
| --- | --- | --- |
| Ordinary domain tools and servers | `Extension::register` and executable tool calls already let extensions own their runtime/processes. | No domain-specific kernel is needed. Retained owner/revocation behavior still needs qualification for services that outlive a request. |
| Native tool wrappers/delegation | Native `Tool` implementations and broker admission exist; searches of the process methods/dispatch found no host `tools/invoke` or original-implementation delegation service. | **New seam:** resolve a revision-pinned implementation, dispatch through ordinary validation/policy, and preserve original-implementation access for an override. Do not equate dynamic tool registration with native delegation. |
| Mutable tool hooks/results | `extension.rs:81–105` supplies immutable arguments, secondary veto, and post-call observation. `extension_process.rs:8930–9005` binds that contract to RPC, but the after hook receives output text/error status rather than the full structured result. | **New seam:** pre-admission argument transformation and an explicit result-projection boundary. Retain canonical execution/authorization facts; an observer cannot retroactively undo a completed effect. |
| Formatted native writes/edits | `tools/write.rs:165–215`, `tools/edit.rs:217–298`, and `secure_fs.rs:521–575` already prepare original bytes, perform conflict-checked per-file replacement, poll cancellation, and generate hashes/diffs. `PreparedMutation` is crate-private, not an extension capability. | **New seam:** bounded transform proposals tied to the host-prepared target/source; reuse this commit path and compute native output from final bytes. Do not format again after the original tool result was produced or export raw file handles. |
| File-mutation observation | `PostMutationKind` at `extension.rs:282–289` is configuration/resource/migration ingestion. The [retained hook guide](../extensions/HOOK-ENRICHMENT.md) explicitly excludes paths, contents, and a watch feed. | **New seam:** an admitted filesystem mutation receipt/revision projection if needed. The current post-mutation rescan hook is not an LSP document-change feed and cannot be repurposed by naming alone. |
| Fresh background delivery | `RunControl::steer/follow_up`, owner/generation fences, and queued session injection already exist. `session/send_message` at `extension_process.rs:5775` injects assistant/system messages, not Pi custom messages with resource guards. | **Binding plus new seam:** R8's retained delivery/idle wake over existing drivers; resource-dependent delivery additionally needs typed freshness guards checked at application. Session correctness does not establish source-file correctness. |
| Rich global tool observations | `events.rs:340–385` already carries arguments, progress, and structured results. `extension_process.rs:8833–8916` projects tool identity/start and outcome/duration/reason, not those full payloads. | **Missing binding:** bounded ordered subscriptions over existing semantic events, with explicit overflow/resync or terminal failure. Keep the synchronous native observer nonblocking; never wait on subprocess RPC in it. |
| Reentrant tool callbacks | The duplex process reader can receive reverse requests while an extension call is outstanding. That is transport capability, not native tool dispatch or proof a parent waiting on a child can make progress. | **New delegation seam plus driver qualification:** service policy-admitted child calls without waiting on their own parent hook/session borrow. Bind callback lineage and cancellation explicitly; refuse actual cycles, not all same-name native delegation. |
| Mid-stream interruption/continuation | `events.rs:195–244` distinguishes provisional deltas/retry. `RunControl` at `agent.rs:1498–1541` offers follow-up, final-answer-at-boundary, and abort; abort ends the run rather than continuing the same logical turn. | **New engine seam, only if separately requested:** attempt-fenced interruption, partial-output disposition, rule injection, and bounded continuation. Existing retry machinery is useful but does not grant extensions this control. |
| Rich terminal composition | Native line components/renderer and remote-frame validation exist; the root fullscreen dispatch/frontend consumer remained absent in the inspected paths (Section 3). | **Missing live binding, then broader contracts:** finish cached remote UI before slot/editor projections. Domain widget code remains extension-local; transport DTOs are not proof of a working game or editor. |

#### Illustrative native-mutation flow, not a proposed wire contract

For a future approved native wrapper/formatter capability:

1. Resolve the tool/override and pin its implementation revision. A delegation
   token selects the intended original implementation rather than recursing into
   the same override; it never skips policy or grants blanket hook bypass.
2. Run bounded argument transformations, validate the final schema/size/path and
   effect classification, then broker-admit that exact invocation. Today's
   serial path reserves before hooks (`agent.rs:11613–11715`); parallel reads do
   likewise (`:2202–2309`), as does recovery (`:4312–4357`). Mutating arguments
   after those reservations would authorize a different operation.
3. Prepare the target/source under the required authority, then obtain any
   bounded byte-transform proposal against that snapshot. Preparation is not
   assumed side-effect-free: write preparation may create parent directories.
   Such preparation must stay behind mutation admission, or be replaced with a
   genuinely read-only preparation phase. A content-changing proposal needs
   admission bound to its final effective bytes before commit; no mutable Agent
   borrow or host paint lock may be held across an extension callback.
4. Recheck source/owner/cancellation and commit through the existing per-file
   secure mutation path. Generate the native hash/diff/result from the actual
   final bytes. Before-commit cancellation can prevent the write; after-commit
   cancellation cannot truthfully claim no write occurred. A process loss at an
   ambiguous commit boundary must remain indeterminate, not trigger blind replay.
5. Publish the committed receipt/revision, then allow deferred work to depend on
   it. Check declared dependencies again when delivering the result. Specify how
   outside writers are detected: a host-only mutation ledger cannot by itself
   observe shell/editor writes, and a watcher is not proof of a complete history.
   Use stated content/revision validation and document its consistency boundary.
6. For server-originated edits, admit a distinct changeset through normal policy.
   Validate every target and report committed/conflicted/failed/rollback facts;
   per-file atomic replacement is not multi-file transaction atomicity. Rollback
   must not overwrite another writer's subsequent change.

This separates three responsibilities: the extension computes the domain
proposal; the host validates/adopts/commits it; later observers consume committed
facts. A mutable hook return alone cannot supply that transaction boundary.

Future contract probes, **only if that capability is approved**, should cover:

- Argument/path/implementation changes cannot consume old approval; serial,
  parallel-read and recovery paths enforce the same invariant.
- Original-native delegation and a parent awaiting a child callback make
  progress; a genuine dependency cycle fails explicitly.
- Formatting changes the bytes, hash and diff together; a competing write or
  owner replacement between prepare and commit rejects the stale proposal.
- Cancellation on either side of rename and process loss do not fabricate an
  unperformed write or repeat an uncertain mutation.
- A late diagnostic is rejected after a newer write, including an outside writer
  within the declared freshness model; right-session/wrong-file results fail.
- Server-originated multi-file edits disclose partial completion and rollback
  conflicts without clobbering another writer.
- Stream interruption racing retry/session replacement has exactly one attempt
  disposition and never executes a tool from discarded provisional output.

These are additional design/reference probes, not changes to R1–R12 or the
current implementation sequence. No new mutation/delegation/stream-control
service or LSP, DAP, eval, or collaboration feature is implemented by this map.
