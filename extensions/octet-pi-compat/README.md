# Pi extension compatibility

**The Pi adapter for octet v0.10.0, octet's first Pi-compatible release.**
Import your existing Pi setup (reviewed managed Pi 1.0.2 and 1.5.0 manifests) or select
extension entrypoints manually.
One Node process loads unchanged extension factories; Rust owns the agent,
sessions, approvals, terminal, keyboard focus and shutdown. The installed-Pi
fallback loads helpers in that process, not a second Pi agent.

See [getting started](../../docs/pi-compatibility.md) and the
[API coverage ledger](../../docs/pi-extension-api.md). Extension execution uses
your OS permissions; it is not a sandbox.
See [native-mediated Pi settings](SETTINGS.md) for the bounded
`SettingsManager.create` contract and its native policy requirements.

## Local setup and configuration

Requires Node **22.19.0 or newer**. Entrypoints reject older Node versions before
loading adapter dependencies or factories. Bun's existing path is retained but
separately unqualified. Source dependency setup uses npm; release archives
include locked production dependencies. Installation/discovery are inert.
Enabling the stock bundle automatically mirrors this
machine's enabled Pi setup and runs its factories with OS permissions. Native
provider-credential disclosure remains a separate reviewed opt-in, and native
TOML appearance is independent. With octet 0.10.0 or later:

```sh
octet extension install octet-pi-compat
# Only after reviewing this machine's enabled Pi factories and imports:
octet --enable-extension octet-pi-compat
```

For source development, the commands below generate a separate reviewed bridge;
neither helper enables it or writes host configuration/trust:

```sh
node extensions/octet-pi-compat/setup.mjs
node extensions/octet-pi-compat/configure.mjs --reviewed --from-pi \
  --output /absolute/local-extensions/octet-pi-compat
# Add --provider-credentials only after explicitly reviewing that extra grant.
# Explicit activation is a separate host/user decision:
octet --extension-dir /absolute/local-extensions --enable-extension octet-pi-compat
```

`--from-pi` uses the installed Pi's own resolver, probes each enabled factory on
octet's emulated path, then automatically tries installed Pi when loading fails
or a bounded source scan finds an installed-only helper. Tool factories on both
routes use the same native broker; direct execution is not an installed-route grant.
Setup prints each route or skip reason and records the Pi installation path.
The recorded agent directory also supplies `PI_CODING_AGENT_DIR` before imports
and factory execution, overriding ambient directory selectors when HOME changes.
This maps reviewed configuration inside the adapter, not native environment passthrough.
Broken factories and unsupported schemas are diagnosed; constraints are never
stripped to make a tool load. Static registration collisions require explicit
ownership and fresh review, refusing a partial setup rather than silently dropping
other commands. Installed native manifests alone do not prove active ownership;
only explicit host-active tools conflict. Reviewed builtin tool replacements require
exact manifest grants and negotiated native `builtin_tool_overrides_v1` admission.
For a manual list, replace `--from-pi` with absolute entrypoint paths. Configure
records the executable; selecting Bun does not qualify that runtime. A load pass
does not qualify commands, hooks or UI behavior.

Runner and configure enable the best-effort private Node compile cache before
loading the adapter graph. Existing `NODE_COMPILE_CACHE` overrides remain authoritative;
unavailable caches do not block startup. Review capture and live factory execution
remain separate and unchanged.

`--from-pi` also snapshots enabled Pi palettes (plus Pi dark/light) under `themes/`
with safe `pi-*` native selectors. Enabling this reviewed bridge contributes these
regular themes to `/theme` and makes Pi's selected palette the session startup
preference, even when host settings save `theme = "dark"`. `--theme`, `OCTET_THEME`,
and later `/theme` choices win for native appearance; Auto/Light/Dark/Cards/Still remain available. No
host or Pi settings are rewritten. `octet-config.toml` and printed theme arguments
are optional standalone alternatives, not extra steps for the bridge launch above.
Re-import to refresh these snapshots. Every newly configured, enabled bridge also
contributes current enabled Pi palette files to `/theme`, including manually
configured nonmirror bridges. Native validation rejects malformed winning files
without exposing lower-precedence shadows. This reads palette data; it does not
enable factories, load skills or apply setup preferences outside mirror mode.
Existing manifests must be regenerated to declare `resources_discover`.

Octet's built-in `pi` selector generates Pi 1.0.2 system colors natively, without
Node or this bridge. Use `--theme pi` or `/theme pi`; terminal colors refresh through
the existing input owner, and geometry remains native. Importing terminal-dependent
system/automatic snapshots is still diagnosed rather than guessed.

The selected JSON snapshot also initializes the bridge's Pi Theme **before**
route probes, registration capture and factory loading. `ctx.ui.theme`, remote
editors/footers, helper callbacks and custom renderers share it; admitted installed-Pi
helpers use the same palette without calling Pi `initTheme` or starting a watcher.
Palette rebinding retires renderer caches and invalidates live component frames.
The interactive native host supplies callback-free palette snapshots and theme
selection through the admitted frontend. Native `--theme` and later `/theme`
choices remain authoritative; the built-in `pi` snapshot preserves all Pi tokens
without substituting model-family colors. This is palette projection, not copied
Pi renderer geometry or full original-package qualification.

Failing lifecycle observation callbacks are reported with their factory and event
while later callbacks continue. Permission/veto hooks and owner fences continue
to fail closed; cancellation remains enforced. A later callback timeout does not
discard host-operation failures already recorded at the live boundary, but an
operation failure arriving after that boundary settles cannot retroactively
change its response. Callback wait budgets do not bound a subsequent flush of
pending tracked host operations, transport or synchronously blocked JavaScript;
the native deadline can still expire. See [slow or stalled extensions](../../docs/pi-compatibility.md#slow-or-stalled-extensions). This is
isolation, not API parity. Print and JSON output both use the native `print`
context mode, without a UI.

`--reviewed` authorizes executing the selected factories and their imports during
routing and capture, including installed Pi code on the fallback route. Review
their normal OS effects first. Configure
writes the requested directory's `extension.toml` and `bridge.json` (and, with
`--from-pi`, normalized theme snapshots and an optional appearance snippet), uses the
canonical Node/Bun executable and this package's absolute runner path, and refuses
existing files unless `--overwrite` is explicit. It does not copy factories or
their dependencies. Install a factory's own dependencies separately.
The generated manifest is local/unpackaged; publishing an installable bundle also
requires the host's exact `requires_octet` pin and independently reviewed packaging.
The 0.10.0 release catalog includes this adapter. For an installed bundle,
generate configuration in a separate directory rather than
overwriting integrity-checked bundle files; see [Bring your Pi extensions](../../docs/pi-compatibility.md#bring-your-pi-extensions).

The runner accepts `node runner.mjs --config /absolute/bridge.json`, with:

```json
{"extensions":["/absolute/reviewed/extension.ts"]}
```

Generated config additionally retains entrypoint SHA-256, bounded literal-relative
import hashes and exact static registration metadata. Changed reviewed sources or
static registrations require explicit reconfiguration; startup never recaptures
unreviewed tools, commands, shortcuts or flags. A multi-factory bridge reports and
skips a changed factory. Late tools use the negotiated host-acknowledged catalog instead.
Static shortcut declarations stay reserved when a factory is
skipped, but their missing handlers refuse execution; reconfigure to remove them.
These hashes are change detection, not a sandbox or
an integrity claim over an extension's entire import graph. The stock `bridge.json`
is empty, but its manifest sets `OCTET_PI_AUTO_MIRROR=1`: explicit enablement
prepares a private session catalog and discovers/runs enabled Pi factories.
An empty config therefore does not mean stock activation is inert. Installation
and discovery alone still execute nothing and never import credentials.

The adapter follows the reviewed subset of **Pi 1.5.0's extension API**. Shipped
capability manifests describe **1.0.2 and 1.5.0**, with 1.5.0 as the emulated
surface. Managed stable versions `>=1.0.2 <2.0.0` are admitted; a version without
an exact manifest uses the nearest older manifest and diagnoses its unverified
classification (1.4.x uses 1.0.2). This is not full SDK or original-package
qualification. Pinned dependencies remain jiti 2.7.0 and `typebox` 1.3.27, plus
selected MIT Pi TUI 1.0.2 modules. Imports resolve through the per-release
manifest's route map, matching Pi's `virtual-modules.js` extension loader:
`@sinclair/typebox` maps onto the same TypeBox 1.x as `typebox`, the `pi-ai`
root, `pi-ai/compat` and `pi-ai/models` share one entry, and both
`@earendil-works` and old `@mariozechner` Pi imports alias to the same host
facades. `pi-agent-core` resolves to explicit host-owned-runtime refusals;
its type-only imports work without starting a second agent. `pi-ai/providers/all`
provides static model inventory and guarded native-provider delegates.
`pi-ai/oauth` remains installed-Pi-only.
Setup uses `npm ci --ignore-scripts`, without global installs, asset downloads,
providers or model calls. There is no coding-agent npm dependency. jiti keeps
its default content-hashed transpile cache, as in Pi. Pure context helpers and
command-argument parsing/path normalization are adapted from Pi 1.0.2 (tag
`v1.0.2`, commit `cd32f7725fdbddbaecdff5b1e68491563394e0ca`) under its [MIT
license](LICENSE.pi); emulated helpers do not import Pi's session store or agent
runtime. The [Pi 1.0.2 export inventory](shims/pi-1.0.2/README.md) records
all public root/compat/core/TUI names, their pure/local or host binding, and
explicit refusals. The [Pi 1.5.0 notes](shims/pi-1.5.0/README.md) describe the
reviewed helper removals, TypeBox moves and argument-validation changes.
Additional reviewed local helpers lazily load individual
installed Pi dist modules and require a supported local/managed installation.
Static `getModel`/`getModels` describe Pi's generated catalog, not native selected
or authenticated models. The explicit installed-Pi route loads the managed Pi's own packages.
Its native ESM imports finish sequentially before the synchronous overlays are
published; an in-flight package graph is never read with `require()`. The shim
precedence and denied-export boundaries are unchanged.

Automatic-mirror catalog reuse does not retain exclusions caused by missing
factory dependencies, unavailable installed-Pi modules or timed-out setup. Those
factories are retried on the next launch, even if their sources did not change.
Dependencies are never installed automatically. Automatic mirrors pin the live
runner to the same persistent transform/compile-cache directory as capture,
rather than a generated per-launch directory. Cold automatic capture also covers
single-factory setups and selects the already-required installed-Pi route for
statically detected installed-only helper imports before running any factory. A successful
mixed capture can therefore supply the catalog without replaying the entire setup.
If an executed factory fails, accepted-only recapture still starts in a clean
process so its shared mutations cannot contaminate the reviewed catalog. The live
runner still loads accepted factories separately, after native admission.
Interactive deferred startup captures the cold catalog in owned background work,
so returning the native fleet no longer waits for Pi factory capture. Until Pi's
hook catalog is known, prompt submission waits rather than skipping its hooks;
activation still requires the validated catalog and original source/trust identity.
Retiring the fleet aborts capture. Print/RPC/headless startup stays synchronous.
These ordering changes and work reductions do not qualify cold-start parity:
actual first-frame and fully initialized timings still require measured journeys.

`ctx.isProjectTrusted()` requires the authoritative native trust snapshot, then
applies Pi 1.0.2's no-project-resources rule for already enabled factories: a newly
bound plain repo can report true if it has no trust-requiring `.pi` entries or
project/ancestor `.agents/skills` (user-global `~/.agents/skills` is excluded).
An added resource can narrow that signal; removing it cannot silently restore the
signal within the same owner, and native trust revocation takes precedence. A
replacement owner reevaluates it. This conservative lifecycle behavior differs
from Pi's retained trust setting. Extensions may start language servers there,
including servers that execute project code. Only the Pi context signal changes:
mirror discovery, `pi.getSettings()`, native resource admission, tool policy and
native workspace trust keep their original host gate.

## Implemented surface

- Pi's own plain-object call semantics for the objects a factory is handed. The
  `pi` object, `ctx`, `ctx.ui`, `pi.events` and every callback event payload
  report an optional member exactly as Pi does: reading a member this build does
  not define yields `undefined`, so a factory can detect a field or an API from
  another Pi release (`ctx.goalStorageRoot !== undefined`,
  `pi.events[channel]`, a later event field) without the callback failing.
  Assigning such a member is refused, because it would silently shadow the API.
  Members Pi 1.0.2 has but Octet deliberately does not honor stay explicit
  refusing functions (`ctx.abort`, `ctx.shutdown`,
  `pi.events.clear`, the unemulated provider/session factories), so an
  unsupported *operation* still fails loudly. Extension-owned Symbol metadata on
  `ctx.ui` survives callback boundaries within one host-issued session owner, so
  footer ownership tokens can be checked and cleared. It is not inherited by a
  replacement owner; action methods keep their own request and cancellation fences.
  Octet-built data objects (observed
  assistant messages and their usage, native receipts, stream options) keep the
  opposite rule: an unavailable fact refuses instead of reading as absent.
- Pi 1.0.2 `config.ts` package helpers (`getPackageDir`, `getPackageJsonPath`,
  `getReadmePath`, `getDocsPath`, `getExamplesPath`) resolved against the
  package that provides the `pi-coding-agent` module: this adapter package on
  the emulated path, the managed Pi release on the installed-Pi route. Path
  helpers do not guarantee that documentation/example directories are shipped.
- Pi 1.0.2 `CURRENT_SESSION_VERSION` (3) and the `keybinding-hints` helpers
  `keyText`, `keyHint` and `rawKeyHint`, implemented from Pi's source against
  the pinned Pi TUI keybindings (host-supplied bindings on the installed route).
- The read-only keybinding managers supplied to custom UI components and returned
  by Pi TUI's `getKeybindings()` implement `matches`, `getKeys`,
  `getResolvedBindings` and Pi coding-agent's `getEffectiveConfig`; the latter
  two return the native host's complete effective snapshot, including overrides
  and reloads. Pi's remaining instance methods are
  explicit gaps on this injected facade: `getDefinition` and `getConflicts`
  need definitions/override provenance not supplied by the host, and
  `getUserBindings` reads as an absent optional member (extensions probe it with
  `getUserBindings?.()`; throwing there took the host down on pi-mcp-adapter's `/mcp`);
  `setUserBindings` and `reload` would contradict host ownership and are not
  supported. This does not restrict extensions constructing their own standalone
  `KeybindingsManager` from the pinned TUI package.
- Pure `calculateContextTokens`, `estimateTokens`, and `buildSessionContext`
  exports on both coding-agent aliases. They operate on caller-supplied Pi data,
  including compaction-aware branches and context edits; they neither transform
  live native provider requests nor read/write the canonical session.
- `registerTool`, `registerCommand`, `registerShortcut`, `registerFlag`,
  synchronous `getFlag`, and ordered `on` callbacks. Initial names/schemas are
  captured before activation. Late tool registration/replacement requires
  `dynamic_tools` and waits synchronously for the native catalog ACK; refusal
  leaves the accepted tool unchanged. Late `on` callbacks can use already
  subscribed native hooks. Late commands additionally require `runtime_commands`
  and `runtime_command_registration_v1`; the owner-fenced synchronous native ACK
  precedes the local callback/catalog update. Late shortcut names require
  `shortcuts` and the existing owner-bound `shortcut/register` native ACK before
  installing their trigger handler. Native key conflicts, rescue bindings,
  quotas and generation checks remain authoritative. Mirrors retain their
  reviewed static-registration fence; new flag names remain startup-bound.
  Unknown command options are ignored;
  only Pi's description, handler and argument-completion options are consumed.
- Reviewed builtin overrides use exact `capabilities.builtin_tool_overrides`
  grants for captured names among `read`, `edit`, `write`, `bash` and
  `powershell`. Startup and late registrations must match the native initialize
  grant and negotiated feature; `bridge.json` cannot grant authority. The
  owner-scoped replacement keeps extension effects, scheduling, metering and
  unsafe replay classification, not the displaced builtin's privileges. Native
  retirement restores the builtin without retargeting frozen calls or replaying
  unsafe historical calls. First-party external tool conflicts remain refused.
- `createReadTool`, `createWriteTool`, `createEditTool`, `createBashTool`,
  `createPowerShellTool` and their `*ToolDefinition` counterparts construct local
  Pi metadata without an installation. Execution requires an admitted live tool
  parent and `tool_composition_v1` plus `composition_builtin_tools_v1`. Native
  dispatch selects the frozen genuine builtin, not an extension replacement with
  the same name, and retains schema, hooks, effects, scheduling, cancellation,
  journaling and metering. Pi cwd/path and shell timeout arguments are translated;
  private native read/shell payloads supply Pi text and structured output without
  exposing native numbered read presentation. Native result-hook replacement wins.
  No raw installed-Pi tool execute function is exported. grep/find/ls definitions
  construct, but execution refuses because those native builtins do not exist.
- The host-owned `ExtensionRunner` instance participates in live registration
  catalog refresh and execution. Interception sees real definition/anchor and
  source identities; captured execute functions are native-admitted proxies.
  `emitToolCall` uses negotiated `composition_preflight_v1` to run real native
  preparation/schema/hooks without effects, mutates the Pi input to the prepared
  arguments, and binds a bounded single-use receipt. Execution must match that
  receipt; `emitToolResult` returns the settled native result without rerunning
  hooks. Construction of an independent runner and execution outside a live tool
  remain refused. This does not mediate arbitrary extension-owned filesystem
  implementations: Fabric's custom preview-write execute replaces the factory
  implementation and remains an unqualified path, not a native builtin write.
- Real sync/async `getArgumentCompletions` callbacks through negotiated native
  `autocomplete`. Registration follows initialization and requires admission.
  Callbacks receive Pi's complete raw argument prefix, with whitespace/quotes and
  Unicode preserved; native UTF-8 cursor offsets are validated, not treated as JS
  string indices. Pi attachment-token precedence is preserved. Results are bounded
  to 32 items / 1024 UTF-8 bytes per prefix/value/label/description, without controls.
  Unclaimed commands and null/empty callbacks return no items; exceptions, overflow,
  unsupported fields and unrepresentable quote/cursor edits fail explicitly.
- Tool `promptSnippet` / `promptGuidelines` map verbatim to tool-only catalog
  `prompt_snippet` / `prompt_guidelines` when `tool_prompt_metadata_v1` is negotiated.
  Declaring either field, even empty, requires the feature. Limits: 1024 UTF-8 bytes
  per string, at most 16 guidelines, C0/C1 forbidden except newline/tab. Pi projection
  trimming and aggregate catalog-budget enforcement remain native responsibilities.
- Synchronous `pi.getCommands()` requires native `command_catalog_v1`: resolved
  extension commands, then admitted prompts, then **available**, not activated,
  skills named `skill:<name>`, with original Pi `sourceInfo` where imported.
  Built-in slash commands are excluded. Membership comes only from native
  registries; the adapter restores its own factory provenance without adding
  entries. Retained same-owner callbacks request a fresh snapshot; unbound,
  retired, cancelled, draining or unsettled-resource contexts refuse rather than
  inventing an empty catalog. Complete results are bounded to 512 KiB. See the
  [native contract](../../docs/extensions/resource-paths.md#available-command-catalog).
- Adapter-side `resources_discover` behind `resource_paths_v1`: ordered awaited
  factory callbacks, normalized filesystem roots and the dedicated native reply.
  Native activation must remain off without the complete App consumer described
  below. Registration capture never invokes resource discovery.
- A shared in-process `pi.events.on/once/off/emit` bus preserving synchronous
  order, object/function identity and retained owner contexts across factories.
- Text/image tool results, `details` retained in `metadata.pi_details`, explicit
  error results, structured outputs and cancellation signals. Optional undefined
  object members are omitted only in final, progress and hook-result tool details;
  explicit null survives. Schemas, structured content, other JSON contracts and
  explicit undefined array elements remain strict. Images are published through
  the verified native artifact service, never caller-selected host paths.
  `onUpdate` snapshots preserve text/images/details/structured content in a bounded
  native ephemeral result channel; late callbacks are ignored. Nested callbacks
  use the same bounded request-local channel and never publish into the transcript.
  Synchronous `ctx.tools` reads the actual frozen native composition catalog;
  `ctx.executeTool` requests host-issued full outcomes through normal admission.
  See [tool schema and exposure contracts](SCHEMAS-AND-EXPOSURE.md) for the bounded
  keyword profile and distinct model/callable surfaces.
  Nested recursion, aggregate usage conversion, parallel/exposure behavior, and
  complete Pi execution-event parity remain unqualified; adapter tests alone do
  not qualify this native integration.
- `pi.exec` may start from an extension callback after its originating request settles
  while that session remains the current owner. Execution stays host-supervised: the
  host enforces process/effect policy, attribution, argument/output bounds, optional
  timeouts and cancellation; stale owners cannot start new executions.
- Owner-bound `cwd`, `model` (truthful `model_view` conversion, including exact
  rate units), session name/ID, and optional host-supplied context usage, entries,
  branch/model/auth-status snapshots. Missing snapshots throw explicit
  `unsupported_feature`, not invented empty session data or credentials.
- Host notifications, active-parent confirmation/input, real remote selection
  and editor dialogs. Notifications preserve the Pi package display name (or a
  humanized package name) through the negotiated `notification_source_v1` contract.
  Useful composer/session-entry/user-message/active-tool operations are available
  only when negotiated. Setters update local mirrors synchronously, await host
  acknowledgement at live boundaries, roll back refused mirror writes, and report
  background refusals. `appendEntry` instead updates only after a known synchronous
  durable receipt (see below). Raw assistant/system message injection remains
  unsupported; custom transcript presentation has its own negotiated gate described
  below.
- `ctx.ui.onTerminalInput` registers a real pre-editor listener: after native
reserved actions and the open slash popup, listeners run synchronously in registration
order over the raw terminal spelling, and a listener can consume input
(`{consume:true}`, including an empty replacement) or replace it (`{data}`).
The whole chain is bounded to 50 ms per event; a listener that does not answer
is latched off (with one bounded diagnostic) and that event is delivered
unchanged, so a stuck factory cannot freeze typing. A listener that throws is
reported once and keeps its subscription, and the original input still reaches
the host. Ctrl+G is reserved only for explicit fullscreen rescue, Ctrl+D stays
host-owned, an active native search query bypasses listeners, and input beyond the existing
256-byte wire bound bypasses the chain. See
[`docs/pi-extension-api.md`](../../docs/pi-extension-api.md) row 23 for the
deliberate differences from Pi's unbounded, blocking chain.
- `ctx.ui.custom`, footer/header/widget/editor factories, safe component focus,
  input listeners and overlay composition. Custom factories receive a read-only
  snapshot of the actual native keybindings, including user overrides and reloads;
  missing snapshots/actions refuse rather than inventing Pi defaults. Host Ctrl+D
  and fullscreen Ctrl+G rescue remain reserved. Editor components occupy only the
  composer slot; native chrome and slash discovery stay visible. JS components,
  callbacks and timers stay live in this process. Only printable text and bounded safe SGR snapshots
  cross the wire; the adapter strips balanced, bounded file/http/https OSC 8 link wrappers and keeps
  their visible labels as plain text. Other terminal escapes remain rejected. Oversized Pi render
  snapshots are clipped to the existing row, per-line and aggregate wire bounds at UTF-8/SGR
  boundaries, with an ellipsis. Rust paints cached lines and never calls JS synchronously.
- `setStatus` retains footer-provider metadata and, without a custom Pi footer,
  projects one width-clipped status row below the native footer through an
  owner-fenced `below_editor` slot. Keys sort deterministically; setting an
  existing key replaces it and `undefined` clears it. A custom Pi footer owns
  status presentation without a duplicate row; clearing it restores ambient
  statuses. Limits are 64 entries per owner, 64-byte keys and 4096-byte text;
  newlines become spaces and only safe frame text/SGR is admitted. Headless mode
  retains metadata without opening a surface. Reload/owner retirement removes
  the old slot through ordinary remote-UI fences. The compatibility palette is
  local, not a claim to reproduce octet's host theme; unknown roles are explicit
  errors.
- Selected Pi TUI components/utilities, with `Editor` and `CustomEditor` sharing
  a synchronous native-backed facade rather than Pi's editing class. Rust owns
  text/caret/undo/history/paste and autocomplete state; JS retains presentation,
  hooks and provider callback handles. `new TUI`, `ProcessTerminal`, terminal image escapes and
  provider/auth storage have no SDK fallback and remain explicitly unsupported.
  `createAgentSession` and coding-tool descriptors now delegate through the
  installed host-owned child facade and negotiated agent-session features only.
  Child calls retain the original host-issued owner and retire with that owner.
- Custom editors require a host-issued mount ID and input revisions. Ordered
  draft checkpoints carry mount/input/checkpoint identity and require matching
  commit replies. A completed input event checkpoints final text even for cursor,
  consumed or release events; intermediate onChange calls cannot acknowledge it.
  Frames capture rendered lines and the exact checkpoint barrier together, then
  wait for that ACK before publishing. Clear/replacement drains the ordered tail;
  observed rescue/shutdown stays immediate. Old hosts without `editor_mount_id`
  explicitly refuse custom editors. During mount/seed initialization, at most 128
  normalized, fenced host events await their first component delivery. The actual
  open reply establishes the mount identity; seeding finishes before ordered input
  dispatch, and the first component frame awaits its captured checkpoint ACK.
  Rescue discards undispatched events immediately, never replaying them into the
  native editor. Original rainbow paced and immediate-burst native qualification
  is recorded below; arbitrary editors remain unqualified.
- Only fenced `composer/set` editor checkpoints from the mounted editor context
  have independent local lifetime while the opening request is live. Other reverse
  calls retain their live-parent attachment. The real parent ID, full owner and
  AbortSignal stay intact. Genuine request/child cancellation still aborts work,
  including cancellation after the opening reply. Nonretained operations require
  the actual live request; numeric ID reuse cannot revive an old captured context.
  Native admission implements the corresponding narrow checkpoint-lifetime rule;
  adapter tests alone cannot qualify it.
- Same-owner `setEditorText` and `pasteToEditor`, including cross-factory callers,
  mutate the active custom component and await its fenced checkpoint. Paste uses
  the component's real cursor API, never an invented append. Without a custom
  editor, ordinary native composer/set/insert behavior is unchanged.

Hook registration maps `session_start`/`session_end`/`session_shutdown` to paired
API 0.4 lifecycle hooks; `session_shutdown.reason` is Pi's `reload` when the host
ends the session for a reload and `quit` otherwise. `tool_call`/`tool_result` map to tool hooks, and
`input`/`before_agent_start`/`after_response` to available prompt/response hooks.
`turn_start`/`turn_end` use awaited native `model_turn_start`/`model_turn_end`
callbacks with a real session-leaf consumer, not whole-run `turn/started` or
`turn/settled` notifications. Whole-run observations remain `agent_start`/`agent_end`.
Message observations now carry `message`, accumulated text updates carry
`assistantMessageEvent`, and `agent_end.messages` contains the run-local observed
messages. A member Pi does not declare on these events reads as absent, as in Pi;
unavailable assistant facts inside a message still refuse. Message and `agent_end` observers request native `before_prompt` and
`model_turn_end` hooks even without a Pi `turn_end` subscriber. With negotiated
`committed_message_events_v1`, system/user boundaries precede the first request,
provider tool-call argument updates are forwarded, and the durable assistant
message ends before its tools execute. Root tool execution events carry actual
arguments, partial results, final results and error status; each tool-result
message follows its execution end. The later model-turn hook supplies the
committed entries without emitting these messages twice. Older peers retain the
model-turn-end fallback.
Tool-result media stays inside its Pi tool result message, which is what Pi 1.0.2
itself holds, even though chat-completions lowering commits it as
protocol-adjacent media beside the result: only the provider wire splits it into
a follow-up user message. Context replay keeps that wire split.
The assistant's exact persisted usage record, linked by entry ID, supplies final
usage, available cost and supported stop reasons to `message_end`, `turn_end` and
`agent_end`. Current model selection and aggregate session usage are never used
for these final facts. Custom message payloads pass through unchanged.

A stream starts with Pi 1.0.2's provisional zero usage and `pending` stop reason.
When no committed observation is available (including projection failures),
settlement removes those placeholders and selected-model identity rather than
publishing them as final facts. Reading unavailable final fields fails explicitly.
Whole-run failure/cancellation is not an assistant provider stop reason: a run
can fail after a successful response. Cancelled runs still settle an open partial.
History is bounded to 8192 messages / 4 MiB; overflow is reported rather than
returning a truncated transcript.

This is not complete Pi event parity: subscriptions determine which facts the
native host supplies, user/stream timestamps are adapter observation times, and
older peers without the committed-message profile still deliver tool-result
boundaries at model-turn end. Message-end replacement and boundary continuation
transformations remain incomplete. Missing final usage/provider/stop metadata is
not invented.
Other negotiated tool/compaction/model/dialog observations are dispatched in order.
Reading unavailable Pi fields or returning an unapplied transformation/veto fails
explicitly. Command argument arrays join with spaces; the wire cannot reconstruct
original shell quoting.

A synchronous reverse request that the host drops (it cancels the hook together
with the child request it issued) no longer takes the extension down. A dropped
*read-only* request (`commands/snapshot`, `tools/snapshot`, `composition/context`) is terminal
cancellation: the factory thread is woken with `request cancelled` instead of
waiting on a reply that will never arrive. A durable or effectful request keeps
waiting for its native outcome, because only that outcome decides whether the
append/registration committed. Replies that arrive after a wait settled are
dropped, and the connection keeps serving later hooks. Only a local deadline
stays an unknown outcome, which terminalizes the connection and is never
replayed.

**pi-clm 1.0.0 is not qualified.** Unchanged pinned registration capture now
succeeds with native per-model-turn mappings; no whole-run aliases are installed
to force registration through.
`before_agent_start` uses the negotiated native `before_prompt_state_v1`
applied-system contract; configure records `capabilities.system_prompt` for that
factory. Handlers share normalized mutable `systemPromptOptions`, including the
available skills and active-tool hints. An edited `selectedTools` list wins over
`setActiveTools()`; otherwise the live loadout wins. The final prompt uses the
host-acknowledged selection, not unknown or hidden requested tool names.
`event.systemPrompt` and the hook's `ctx.getSystemPrompt()` render current
mutations lazily; a returned scalar prompt
sets `forceSystemPrompt`, including an empty string. Named sections replace an
existing matching section or append in Pi order, without persisting native
session state. Unchanged inputs retain the native base prompt. Tool-call hook
blocks skip `tool_result` transformation while retaining error execution/result
observations. MCP connection or capability failures are sanitized background
diagnostics, not failed `session_start` callbacks; the registration stays visible.
Registration capture and adapter fixtures do not qualify CLM's seven behavioral
gates. Missing initial snapshots, unsupported projections/preparation
fields and checkpoint bounds remain open compatibility work.

**Rule A:** captured Pi breakages remain Octet-owned defects across the loader,
adapter, protocol and terminal integration. An explicit refusal is safer than fake
success, but is not a repair or closure. Qualification must match the original case,
candidate and security policy; historical passes do not qualify changed source.

## Native context, persistence and provider phases

Outside `before_agent_start`'s lazy prompt composer, `ctx.getSystemPrompt()`
reads the native disclosure service synchronously. With negotiated
`run_system_prompt_read_v1`, `agent_start` and actual model-turn callbacks use
an exact native run scope: the current Agent prompt is immutable for that run,
so the read does not wait on the frontend borrowing the Agent. The lifecycle
observer remains read-only and never acquires a fabricated live parent. Model
callbacks retain their actual live parent/controller checks. Cancelled,
completed, retired or replaced scopes refuse; neither cached host prompt text
nor an older/newer run is a fallback. Idle live requests keep the existing
frontend service. This implementation and its regression tests are not package
or full-Pi qualification.

`ctx.hasPendingMessages()` requires an actual native boolean; absent facts still
refuse. With paired session transport, queue-only `runtime.updated` publications
update an existing live exact owner's retained fact without `remote_ui` or a UI
mount. They cannot change foreground ownership, history or editor/UI authority,
and accept no additional host fields. Delayed publications for unknown or retired
owners are ignored without creating state or reviving action authority; retained
context actions still refuse retired owners. Native queue and original-package
qualification remain separate from this adapter contract.

`context` maps to the real API 0.4 `provider_context` preparation hook and requires
`session_entries`, a matching native owner/preparation and actual top-level
`session_leaf` grant. Ordered awaited callbacks can replace messages or mutate the
list/content in place. The native system stays separate. Only canonical messages
and system are returned, never replacement tools, credentials, route or Session.
Text, tool calls/results and Pi custom messages have explicit translations.
Context callbacks can retain unchanged, provenance-bound OpenAI encrypted reasoning
and Anthropic redacted thinking while editing other messages. The original native
state replays unchanged; copied or modified opaque signatures are refused. Read-only
lifecycle observations confer no replay authority. Unsupported media, continuations
and roles fail rather than lose data. Missing timestamps, usage and historical provider identities are not
invented. Native validation/budgeting remains authoritative.

`appendEntry` is now **synchronous void**: an independent worker owns the sole
physical stdin reader/serialized writer while the factory thread waits for the
native durable receipt. Hook writes require an actual one-use session-leaf grant
or the negotiated grant-less receipt path below. An exhausted or revoked grant
cannot fall back to grant-less writes; only its known successor permits another
append. No optimistic entry or Promise
flush counts as persistence. Cancellation after claim waits for the actual outcome;
transport loss means unknown outcome, terminal connection and no replay. Existing
16 KiB/depth16/nodes256 private-entry bounds remain; large CLM checkpoints can be
explicitly refused. Native entry mirrors translate lazily, preserve actual IDs,
parents and available timestamps, and expose only the initialized namespace's
private custom state. Unrepresentable mixed-message entries refuse; other native
markers retain their identities as `octet_native`, not fabricated Pi entries.

Negotiated API 0.4 `session_append_receipts_v1` requires `session_entries` and
both `session_snapshot_transport_v1`/`session_owner_routes_v1`. Grant-less appends
from live hooks, commands, shortcuts and tools then publish the actual native durable
entry into that invocation's pinned view before returning. Receipt revision,
head, ancestry and view quotas must match; no session-leaf grant is minted.
Immediate sessionManager getters therefore see the committed ID, parent and
native timestamp. Older peers retain the exact entry-ID-only ACK contract.
Adapter receipt tests do not establish native or original-package acceptance.

Negotiated `pipeline_hooks_v1` dispatches actual `before_provider_request`,
`before_provider_headers` and `after_provider_response`. These operate on encoded
JSON, header mutation/deletion patches, and real status/header arrival respectively,
not canonical context or inferred stream completion. Private callback exceptions
are redacted. Native code polices reserved headers and effects before send.

For extension-owned provider streams, negotiated API 0.4
`provider_request_hooks_v1` (requiring `provider_proxy_v1`) relays installed Pi's
actual payload and HTTP-response callbacks through the live stream job. The
attempt stays unclaimed until the real native or callback-bridge route is
selected. Header transforms run at that route's actual outgoing HTTP boundary;
process-owned credential headers stay outside the bridge projection and cannot
be injected through its result. The host retains one hook-chain identity,
reserved-header validation and deadlines;
withdrawal, cancellation and terminal settlement invalidate pending callbacks.
Native delegation and the callback bridge cannot both consume the same attempt.
A callback that omits real wire phases cannot report a successful hooked stream.
No canonical request or synthetic completion substitutes for an HTTP boundary.
Native delegates still refuse package-supplied response/header callbacks that
lack wire forwarding; undefined optional callback fields remain accepted.

`modelRegistry.getAll()` and `find()` use the host's full native and extension
model inventory, including models without configured authentication.
`getAvailable()` is the separately filtered configured-auth inventory, not a
network reachability check. `hasConfiguredAuth()` reads provider-level native
configuration facts without resolving or disclosing credentials. Catalog views
are refreshed defensively; unrepresentable or oversized snapshots refuse rather
than silently dropping models. Extension-owned custom models retain their full
process-local metadata in defensive registry snapshots; private metadata is not
added to the native host projection.

Explicit string-form provider models preserve sparse `thinkingLevelMap` data in
custom-stream and defensive model snapshots. Missing ordinary levels retain Pi's
defaults, null excludes a level, and `xhigh`/`max` require their own non-null
entries. Optional undefined map members are omitted, not converted to null.
Only the resulting portable choices cross into native `ReasoningOptions`;
provider-specific mapped strings stay in the adapter and the custom stream maps
its unchanged portable `options.reasoning` itself. Malformed maps and maps with
no selectable level refuse registration. Inherited native controls remain native.

Provider credentials are withheld by default. Add `--provider-credentials` only
when the reviewed factories need Pi 1.0.2 `modelRegistry.getApiKeyAndHeaders`;
configuration records the separate `provider_credentials` capability, and the
host negotiates it only for that configured extension. Each call resolves exactly
the requested native provider/model. Returned secrets are available to the calling
factory only; they are never included in extension status or host diagnostics.
Request signers that cannot be represented as a Pi API key/header result are
refused. Reconfigure and explicitly enable the capability only after reviewing
all loaded factory code and its imports. `getApiKeyForProvider()` uses this same
opt-in and exact-model broker; missing grants, unavailable providers, refusals or
header-only authentication return `undefined`, not a fabricated key. Custom
providers use only their own authentication callbacks and registered model headers,
without native credential-disclosure permission or fallback to the native broker.
Callback failures return a generic refusal without exposing exception text.
String-form provider registrations can retrieve their own explicitly supplied
key for a declared model without native disclosure permission. These reads use
only the live process-owned registration; caller-supplied headers and undeclared
models cannot select credentials.

When `nested_inference_v1` is negotiated, `modelRegistry.streamSimple()` is a
host-owned, exact-model inference service available only during a live tool call.
Its async iterator and stable `result()` promise expose bounded text/thinking
stream events. Native routing, budgets, cancellation, and durable Session usage
accounting remain authoritative; the selected session model is unchanged.
Caller credentials, URLs, headers, fetch functions, tools, and session controls
are not accepted. Cancellation waits for native settlement, including uncertain
usage exposure when final provider usage is unavailable. This bounded profile
admits one inference per tool execution and does not add usage to tool results.
A tool may return an exact echo of its own settled native inference usage; that
echo is validated against a private invocation-bound receipt and is not charged
again as ToolOutput usage. Usage inside details/structured content is preserved.
Unmatched, additional external, stale or cross-invocation usage remains explicitly
unsupported rather than being silently discarded or combined.

Provider overrides use separately negotiated `provider_inheritance_v1`, offered
only with a native catalog and the provider proxy. Defined registration fields
merge; omitted models inherit the host's native inventory. Single-object native
provider wrappers can select from that inventory. Registration does not resolve
authentication or invoke stream callbacks. Overlays retain the original native
model identity, capabilities and pricing, and removal restores the original route.
Google Generative AI inheritance additionally requires
`provider_google_generative_ai_v1`. Its `google-generative-ai` identity maps only
to the native Google codec (`google_generative_ai`), retaining the native endpoint
and authentication. This does not admit Google Interactions, arbitrary custom
Google HTTP transports, or additional API 0.3 protocols.

Inside an admitted inherited callback, the guarded native transport delegates at
most once through the current native client, retaining its credentials, request
hooks, cancellation and accounting. Credentials are not passed to JavaScript;
this delegation does not require credential-disclosure permission. Context and
payload callbacks may shape messages/system, but cannot replace tools, routing,
authentication, reasoning, cache or session authority. Unchanged native signed or
redacted reasoning retains its host-owned replay state. Native deltas are relayed
before the terminal response; final usage/cost remain native-owned. This is not
permission to run installed Pi HTTP/auth runtimes, perform nested inference, or
claim OAuth-dependent package behavior without an original-package journey.

Negotiating request hooks does not eagerly claim an attempt before the stream
chooses its route. Guarded delegates retain one native wire hook chain; only the
adapter's exact forwarding callbacks are removed from their options. A package
that has already used the callback bridge cannot switch that attempt to native
delegation. Arbitrary response/header callbacks still refuse when the guarded
route cannot wire-forward them. The supplied bridge fetch applies hooks to real
outgoing non-credential headers, leaving process-owned authentication outside the
host projection. Public installed-Pi stream helpers remain installed-Pi helpers;
this routing does not silently replace them with guarded delegates.

Custom stream preparation and inherited native delegation use the bundled pure Pi
transcript helpers and timeout parser, without importing an installed Pi runtime.
An extension that explicitly invokes a managed-local Pi helper still needs a
supported installation; its version and authority checks are unchanged.
`OCTET_STARTUP_TRACE=1` also reports data-free provider preparation/unfold timings
and message/tool counts. These diagnostics are not benchmark qualification.

Custom OpenAI HTTP delegation accepts Pi base URLs with or without a trailing
path slash; only the private native endpoint projection adds the missing slash.
URL-derived Chat defaults and OpenRouter affinity headers use parsed, case-insensitive
hostnames with DNS-label boundaries, not substring matches in paths or unrelated
hosts. Explicit provider IDs and compatibility overrides retain precedence.
Native URL validation, unauthenticated routing and process-owned key isolation
remain unchanged.

Ordinary custom-provider streams receive canonical history when a supplied
Responses replay is exactly equivalent to the native codec's canonical encoding
and contains no additional controls. Opaque or control-bearing Responses replay
is not discarded and remains explicitly unsupported by `streamSimple`.

Actual awaited `session_before_compact`/`session_compact` and
`session_before_tree`/`session_tree` hooks carry native session-leaf consumers.
Cancellation/veto is not an advisory notification. The bounded replacement profile
supports summary + firstKeptEntryId; unsupported counts/details/usage fail explicitly.
Native missing Pi preparation fields (`willRetry`, tokensBefore, settings/fileOps,
full tree-summary preparation) remain explicit refusals, not fabricated values.
`ctx.waitForIdle` requires a live command and negotiated `session_control_v1`, then
awaits the actual same-session idle receipt. No hook can wait for its own run to end.

`ctx.newSession({parentSession, setup, withSession})` replaces the native session
at its idle boundary. A request made while a turn is busy waits for that turn to
settle; it never takes its session writer. The old session's end precedes writable
setup, and the new session's start follows setup's durable writes. Only the
original live command can consume the host's creation receipt, with its exact
process instance/generation and cancellation lifetime. Captured old contexts stay
retired; the setup manager closes before `withSession`. Parent linkage is stored
in the replacement's durable header, not inferred from a foreground UI context.

`ctx.compact({customInstructions?, onComplete?, onError?})` is synchronous void.
It requires both `session_control_v1` and `session_compaction_v1` and an opted-in
native interactive idle driver. From a live command/tool/hook it queues locally,
then sends `session/compact` **only after the successful parent reply is written**;
a failed/cancelled parent cannot dispatch the queued work. It never waits for idle
inside that parent or treats queue admission as success. Retained callbacks keep
the original issued owner. One compaction per owner / eight per process bounds
pending work, including callbacks. Instructions are at most 16 KiB UTF-8, with
only newline/tab controls. Compaction hooks cannot request recursive compaction.

The completion callback follows the real durable checkpoint and its after-hooks;
it receives actual `summary` and `firstKeptEntryId`. A requested completion/error
callback runs in a fresh awaited host `hook/run` with the original owner, a new
numeric request ID and a one-use native session-leaf grant. The settled origin ID
is correlation only and is never append authority. The idle consumer commits
callback appends before returning the terminal `session/compact` receipt.
Unavailable Pi metrics/details fail explicitly when read. A post-commit failure preserves the checkpoint and
must not be retried; cancellation is not rollback. Owner retirement, cancellation,
transport loss and shutdown revoke pending work without retargeting another owner.
The existing 30-second reverse deadline is unchanged. Headless/Serve and Native
Responses are not yet covered by this local cancellable service; those are open
compatibility gaps, not completed parity cases.

Per-model observations carry the actual iteration index and boundary time. End
follows the durable assistant plus its paired tool results, including native async
settlement; retries do not duplicate start. The adapter projects representable text
and tool messages with actual entry timestamps, including batched tool results.
The native `assistant_metadata` payload contains `assistant_entry_id`, the
recorded model, usage, optional native cost and optional stop reason from the
matching persisted `UsageRecord`. The adapter validates entry/model correlation,
preserves token totals and nonzero cache/reasoning subsets, and converts stored
microdollar cost categories plus the total picodollar remainder to Pi dollars.
Unpriced usage remains readable; reading unavailable cost explicitly refuses,
including copied `ctx.sessionManager` entries/branches. A footer that assumes
`message.usage.cost.total` always exists needs a priced model, or must handle
unavailable cost. Unknown subscription pricing is not `$0`; the adapter never
fabricates a price to satisfy footer arithmetic.
`end_turn`, `stop_sequence` and `pause_turn` map to `stop`, `max_tokens` to `length`,
`tool_use` to `toolUse`, and `refusal` to `error` with Pi's generic refusal message.
Deferred/steered/unknown outcomes without a faithful Pi binding are reported,
not converted to successful completion. Missing legacy accounting/stop reasons
and historical provider identity are never invented. Unsupported media/scheduling
projections remain separate gaps. Model-turn callbacks may append private entries
through the live leaf consumer, but cannot veto already observed work or transform
messages. A cancelled/failed turn need not have an end, and auxiliary compaction,
gate and deferred-resume requests are not fabricated as logical model turns.

The native Pi modules and their per-module counts, the adapter suite result and
the recorded real-package matrix are in the
[dated release-status record](../../docs/pi-compat-release-status.md), including
its historical failures and qualification limits. Non-resource factories
also join
deferred startup before command admission through the existing interactive or
headless pump, without loading resources or changing themes. Native regressions
cover startup ordering, exactly-once headless startup and failed-worker refusal.
These do not qualify full provider parity or CLM's seven original-package gates.
`resource_paths/pi_app_tests.rs` also covers ordinary nonempty factories through
the actual App/Agent, without replacing the Agent or using a handwritten protocol
peer; original-package and terminal acceptance remain separate gates.

## Transient MCP registrations (source integration)

`registerMcpServer`, `unregisterMcpServer` and `getMcpServers` are synchronous
Pi 1.0.2 registry operations: validated cloned configs, factory ownership,
replacement order, namespace collision checks and session-only state. Late
changes deliver complete `mcp_servers_change` snapshots. Load-time registrations
are read on `session_start`, without a fabricated change event. A registered Pi
MCP event consumer takes over connection handling; no parallel native connection
is made in that case.

Without a custom consumer, negotiated API 0.4 `mcp_registration_v1` routes
`mcp/replace {parent_request_id, resource_owner, servers}` to the **already
enabled** resident `octet-mcp` BridgeManager. It does not enable/discover/install a
server, write `mcp.json`, or use a JS transport. File-configured namespaces win.
The native manager retains exact argv, sanitized environment, catalog epochs,
policy checks and supervised cleanup. Transient calls require the matching
host-issued bridge owner; originating owner retirement/crash/reload removes the
overlay. Registration acknowledgement is not connection success: catalog
publication and health still belong to the resident bridge.

The current native connection profile is **explicit `exposure: "direct"` stdio**,
with literal environment values, session-relative cwd and add/replace/remove.
Configs outside that profile remain visible to Pi getters/custom consumers and
produce redacted extension diagnostics rather than silently changing meaning.
Remaining gaps: default codemode/deferred/hidden and per-tool exposure, server
prompt descriptions, HTTP/OAuth/provider credentials, `${NAME}`/`!command` and
home expansion, Pi tool namespace spelling (native stable hashed names remain),
and progress-reset timeout semantics. Native package limits still apply (32
adapter registrations; the manager's configured-server, argv, env and timeout
ceilings). Ledger row 21 remains partial for those explicit gaps.
The offline real-App module `extensions/mcp_native_tests.rs` now passes all three
cases: reviewed configure, native discovery, real Agent execution and durable
results, replacement/removal/owner cleanup, headless startup and refusal without
the resident. Tests use a private valid config and disposable HOME. Headless
commands service owner-fenced reverse requests without implicitly acquiring a
composer; explicit UI operations still require a real frontend. Reproduce with:

```sh
cargo test -p octet-coding-agent --lib --locked --offline pi_mcp_native -- --nocapture
```

## Resource discovery boundary

`resources_discover` requires offered/negotiated API 0.4 `resource_paths_v1` whenever
registered. **Do not enable the native feature without the complete active App
consumer and its startup/reload/retirement qualification.** This adapter mapping
alone is not activation or native-loader qualification. Review/capture records the
hook without calling it; changed reviewed catalogs require explicit reconfiguration.

Native `hook/run` supplies `{cwd,reason:"startup"|"reload"}` and a complete
`context.resource_owner`, after its real `session_start` settles. The adapter uses
its existing ordered hook queue, snapshots handlers in factory/registration order,
awaits each callback, and replies only
`{resource_paths:{skill_paths:[],prompt_paths:[],theme_paths:[]}}` with actual results.
There is no generic disposition/context/notification envelope or cached old roots.
Reviewed imported themes precede factory paths in this reply and may include a
`default_theme` file preference, subject to [native admission and selection](../../docs/extensions/resource-paths.md).
Undefined/omitted contributions are empty; null, malformed arrays, sparse arrays,
unknown fields and unsupported transformations fail explicitly. Ordinary callback
exceptions are diagnosed with factory provenance and later callbacks run (Pi policy);
coded adapter/host refusals, validation errors and cancellation fail the request.

Paths use pinned Pi lexical resolution: trim, home/file-URL expansion, then resolve
against **event cwd**, not the factory or manifest directory. Internal Unicode
spaces and `@` are not rewritten; a blank string resolves to cwd just as in Pi.
Synthetic/inline paths are refused. Raw and normalized paths are limited to 4096
UTF-8 bytes each; controls after trim or URL decoding are refused. At most 64 paths
and 64 KiB normalized path bytes are aggregated across all arrays and handlers,
before any native deduplication; order and duplicates are preserved. No filesystem
access, symlink canonicalization, trust grant, theme conversion or precedence
emulation happens here. Native loaders own admission; native later-wins precedence
is not Pi first-skill-wins. Native JSON theme conversion and its qualification are
separate from this adapter; path normalization alone never establishes format support.

Request cancellation and owner retirement abort discovery's own signal, including
queued work, without weakening other hooks or retained editor lifetimes. Late
cooperative callback results cannot publish another reply. Cancellation does not
forcibly stop arbitrary trusted JS. Native deadlines, generation fences before
publication, actual registry/catalog/theme refresh and removal of already-applied
roots are still the native consumer's responsibility; the adapter cannot supply them.

## Command completion boundaries

A completion request is `{text,cursor,revision}` with a UTF-8 byte cursor and a safe
nonnegative revision. One process-owned chain is registered after `initialize`;
no numeric parent or session owner is invented. The chain promise is armed
*before* the initialize reply can settle, because the host may query completions
the instant it reads that line: such a query waits for admission instead of being
refused, while the registration request itself still follows the reply (the host
resolves the negotiated `autocomplete` feature from it). Nothing orders the
host's next inbound request after a write callback, so this ordering is
deliberate, not timing. `completion-registration-order.test.mjs` holds the reply
write to make it deterministic.

Native text/cursor/revision/focus and provider-owner fences reject stale
results before display and acceptance. A custom native facade additionally binds
its primary component handle: queries capture the mount/handle/native revision
and use the actual native caret, including UTF-8/UTF-16 conversion. One native
registry menu owns Up/Down and Tab before component hooks/consumers, preserves
text after the caret, and commits into that same native model. Clear or mount
retirement invalidates outstanding callbacks; a late result cannot revive it.
Requests may finish out of order; the adapter preserves each snapshot/request ID,
not an invented cross-session revision counter. Cancellation terminates the wire
request promptly; a late callback result cannot publish a second reply. Arbitrary
trusted callback code is cooperative and cannot be forcibly preempted.

The current suffix-only wire cannot express consuming a closing quote *after* the
cursor or leaving the cursor *inside* a quoted directory choice. Those cases are
explicit `unsupported_feature` errors, not approximated edits. Tab-containing raw
prefixes still reach callbacks, but positive results cannot pass the native plain
text grammar. There is no Pi filesystem completion implementation in this bridge.
Native chaining/path fallback on an empty or unclaimed result requires its own
snapshot-fenced implementation and qualification; process tests alone do not prove
ordinary path Tab still works. Native trigger/selection behavior is not a claim to
reproduce Pi's automatic-versus-forced-file completion UI.

The native host side of this boundary is now qualified for the bounded case.
Registration is process-owned and admitted without a foreground UI lease, so a
captured factory activates in headless, background and later interactive states.
The interactive input loops observe the composer and start one cancellable 500 ms
query per live draft, so typing retires the previous request instead of queueing
behind it; Up/Down move the displayed menu selection, Tab accepts the fenced
choice, and only an explicit Tab can fall through to native path completion. A
slow callback therefore cannot block typing or overwrite a newer draft. Real-package
observation: unchanged pi-powerline-footer `cd` completions render in the composer
(`../` Parent directory, `~/` Home directory, workspace directories, "extension
suggestions | tab accept") and Tab inserts the selection; the previously failing
pi-ding, pi-powerline-footer and pi-doom factories now activate with no
`host refused autocomplete registration`.

## Remote frontend integration

See `integration-contract.json` and octet's `docs/extensions/remote-ui.md`.
Negotiate `remote_ui` only on API 0.4 when offered; otherwise `ctx.hasUI` is false.
Complete host-issued owner triples fence every retained surface/context.
`context/updated` is `{resource_owner,host}`; unowned/foreign/stale updates cannot
change a retained footer. Existing `ui/editor-state {text,revision,focused}` is
negotiated with `editor_handoff` and mirrors the active owner's native composer
when no custom editor is mounted. It has no mount/input identity and cannot ACK
or overwrite a custom draft, even after its write queue drains. Command-time
`composer/get` refreshes only the native mirror, never a fake empty string. `model_view` snake-case fields map to Pi camelCase fields.

After a custom fullscreen surface's first frame, `command/execute` settles, but
the unchanged JS handler keeps awaiting `custom()` until the component's actual
`done(result)` or host dismissal. That continuation retains its numeric parent
and complete owner for background owner-safe operations. Cosmetic editor/footer/
header/widget setters return void and do not hold the parent reply open. Their
native mounts wait for the successful origin reply and callback outcome, then
recheck owner, cancellation and drain fences; failed or preempted callbacks cannot
mount. Close, resize,
owner replacement, cancellation, EOF and shutdown dispose components and revoke
their timers; Rust remains the restoration authority.

Presses reconstruct legacy raw keys; modifiers/repeats/releases reconstruct Kitty
sequences (including original Doom's `wantsKeyRelease`). Native reserved actions,
open slash-menu keys and native search-query input bypass `onTerminalInput`.
Ctrl+G stays host-reserved only for fullscreen rescue; in a composer slot it is
ordinary consumer/editor input. Ctrl+D is always host-owned. A listener never
observes a protocol reply as input.
`ui/mouse` reconstructs SGR mouse input. The original drawing
extension's exact known `1000/1002/1006` enable/disable writes are interpreted
**only as capture intent** before `ui/open {mouse_capture:true}`; no raw escape is
written to a terminal. A single complete OSC 777 desktop-notification write is
decoded into a bounded `ui/chrome` `desktop_notification` request with the actual
foreground owner. Only the Rust renderer emits the escape, after host admission;
OS delivery depends on terminal support. Embedded controls, oversized text and
headless/unowned calls are refused. Other direct terminal-control writes fail
explicitly.

Stdout is exclusively a captured serialized RPC writer. Console and direct
plain `process.stdout.write` calls are bounded stderr diagnostics. Transport has
1 MiB input/output frames, 128 writer slots/4 MiB queued bytes, latest-wins UI
snapshots, 128 outstanding reverse requests, 65,536 unique reverse IDs, 30-second
reverse deadlines, at most 8 host requests and 16 surfaces. Worker inbound transfer
is bounded to 256 frames/4 MiB (128 while synchronous append is blocked), allowing
an editor's unchanged 128-event initial burst plus control frames. A synchronous
append timeout requests cancellation but waits for the known result; a final
watchdog terminalizes as unknown, never replaying. Cancellation and
reverse replies stay serviceable while ordered hooks await; cooperative JS cannot
preempt CPU-bound code. Shutdown acknowledgement/drain is bounded to 1.5 seconds;
EOF/crash exits and the host owns final process-tree cleanup. Trusted code can
still access OS facilities directly: this isolation is protocol discipline,
not security confinement.

Synchronous replies also carry bounded observations of already-received `ui/closed`
notifications in a separate 32 KiB worker buffer. Only the matching native surface
is marked retired; the ordinary notification retains FIFO ownership of disposal
after the component callback unwinds. Cleanup skips stale final-state reads and
duplicate close requests. This does not reorder notification callbacks, accept stale
native operations or classify arbitrary `-32002` errors as retirement. Incomplete
input still reports failure; normal errors and authoritative reply outcomes remain intact.

### Transcript presentation

Negotiated API 0.4 `transcript_render_v1` requires `remote_ui` and an installed
interactive frontend consumer. Generic runtime configuration stays default-off;
the active native terminal and Tern consume the same immutable presentation
snapshots. This supports tool call/result/shell renderers, `registerToolRenderer`,
`registerMessageRenderer`, `registerEntryRenderer` and
`registerMarkdownTransformer` without render-thread RPC or a second renderer.
Markdown transformations never alter canonical blocks, durable session data or
provider content. Hidden messages stay hidden; private-entry requests are
namespace-filtered before cloning or dispatch. Unrendered private entries remain
invisible.

Work is bounded to four async jobs, a 500 ms aggregate per-job deadline and a
128-frame / 4 MiB presentation cache. Owner/process generation, source/content
revision, geometry, disclosure, theme and session fences reject stale replies.
Failures and older sources outside the recent presentation window keep native
fallback instead of spinning retries. Native driver/cache/hydration and active-Tern
projection tests pass. Real-App acceptance with the actual adapter also passes
controlled native read execution, canonical persistence, invalidation, resize and
active Tern. These cases do not establish complete renderer/UI or corpus parity.

## Verification

```sh
export CARGO_TARGET_DIR=/absolute/existing-target
CARGO_PROFILE_DEV_DEBUG=0 cargo build -p octet-coding-agent --example native-editor-test-host --locked -j3
npm test --prefix extensions/octet-pi-compat
npm run test:editor-native --prefix extensions/octet-pi-compat
# Optional read-only managed Pi 1.0.2 helper/color integration (isolated HOME):
PI_THEME_AGENT_DIR=/absolute/pi-agent node --test extensions/octet-pi-compat/test/runtime-theme.test.mjs
# Optional guarded execution of the unchanged reviewed hashline factory:
PI_FLEET_HASHLINE_PATH=/absolute/pi-hashline-edit-pro/index.ts \
PI_FLEET_AGENT_DIR=/absolute/pi-agent \
  node --test extensions/octet-pi-compat/test/hashline-original.test.mjs \
    extensions/octet-pi-compat/test/builtin-overrides.test.mjs
```

A historical isolated Pi-theme adapter run recorded **932 tests: 912 passed, 0
failed, 20 optional skips**, exit 0, with a freshly built native editor test host
and a read-only Pi reference checkout.
This is synthetic/native-peer evidence, not existing-setup or attended RC
qualification. Those counts describe that run, not the current tree; rerun
`npm test` for current-source evidence. Editor fixtures require the
compiled native test peer above (or `OCTET_NATIVE_EDITOR_TEST_HOST` pointing to it); missing
peers fail explicitly, never substitute a JS model. The immutable 193 upstream cases plus the
separate ownership guard pass **194/194 with zero skips**. See the
[corpus provenance and fixture boundary](test/pi-v1.0.2/README.md): native-service
coverage is distinct from actual App/adapter owner/admission/registry tests and
real-terminal qualification. The earlier two-batch adapter run covered 566 tests
with a disposable HOME: **539 passed, 27 skipped, zero failed**. The separate original-hashline/override run passed all
8 tests: historical installed-route selection, native-style reviewed grants, session-start
active-tool ACKs, real hashed text read, anchored replacement and PNG execution
with verified artifact bytes/digest. Its isolated file/network/child-process
guards reported zero violations. This uses real original code and an adapter
subprocess with a scripted RPC host, not full native App/PTY acceptance.

Deterministic synthetic-host subprocess tests cover registration, isolated output,
owner-retained frames, cancellation/EOF/malformed transport, backpressure, shared
identity, ordered hooks, local mirrors, real choices and safe input/frame grammar.
Completion tests execute actual factory callbacks, admission/cancellation, Unicode
byte ranges, out-of-order replies, quote/cursor refusals and strict result bounds.
Tool-metadata tests execute real tools and validate negotiation, empty declarations,
UTF-8/count/control limits and unchanged manifest shape. These are not native
completion-UI or effective-prompt qualification.
Resource tests execute real factory subprocesses with nonempty paths, deterministic
ordering/cancellation barriers, strict envelopes/bounds, capture equality and a
source-hash-verified Pi 1.0.2 normalization oracle. They do not prove native
filesystem admission, App publication, prompt projection, theme parsing or CLM
gate 1.
Optional acceptance tests load original files unchanged via `PI_DOOM_PATH` /
`PI_DOOM_WAD`, `PI_FOOTER_PATH`, `PI_DRAW_PATH`, and `PI_RAINBOW_PATH`. They skip
when sources/assets are unavailable, never download or copy them, and exercise
real Doom WASM animation/resume, powerline footer snapshots, drawing mouse export,
and an animated CustomEditor. Synthetic-host acceptance does not establish actual
terminal painting. Recorded Rust/frontend PTYs pass unchanged Doom including
resize, close and terminal restoration without follow-up input; the powerline
footer passed in earlier recorded runs and timed out in every 2026-10-06 matrix
run, and the host now answers the model-auth query that probe needs, so re-run
the probe on your binary to confirm. Original `/draw`
acceptance remains historical; these are observed working-tree binaries, not a
frozen release candidate.

Before the composer-slot change, the unchanged original rainbow editor passed
both paced input (with a grace
period) and strict `--burst` input (without one) on a freshly built native CLI.
Both probes verify the host mount acknowledgement, matching Ctrl+G retirement,
a complete native editable draft in a newer frame, a later native edit, resize
and shutdown/terminal restoration. This closes the original immediate-rescue
draft-loss regression, not arbitrary-editor or full Pi parity. Historical red
burst evidence remains intact. Those retirement probes do not qualify the current
composer-slot contract, where Ctrl+G must retain the editor.

The checkpoint protocol intentionally refuses historical hosts without mount/input
fences. Recovery still exposes the last host-committed draft, not a promise that
unacknowledged input was recovered; stale effects cannot overwrite later native
edits. Startup/Pi runtime performance parity and Doom aspect ratio are not
established by these editor probes.

```sh
# Current composer-slot contract: native chrome/slash popup and Ctrl+G retention.
python3 scripts/test-pi-composer-slot.py /absolute/octet \
  --footer /absolute/reviewed/powerline-footer/index.ts \
  --editor /absolute/reviewed/custom-editor.ts --output /absolute/private-evidence
# Optional offline comparison against source-extracted pure Pi 1.0.2 functions:
PI_REFERENCE_REPO=/absolute/reviewed/pi-checkout \
  node --test extensions/octet-pi-compat/test/context.test.mjs
# Source-hash-verified pinned commands.ts and Pi1.0 completion parser/apply oracle:
PI_COMMANDS_PATH=/absolute/reviewed/pi-1.0-commands.ts \
PI_REFERENCE_REPO=/absolute/reviewed/pi-checkout \
  node --test extensions/octet-pi-compat/test/completions.test.mjs
# Unchanged registration capture only, NOT a CLM behavioral parity pass:
PI_CLM_PATH=/absolute/reviewed/pi-clm-b84a9d7c/index.ts \
  node --test extensions/octet-pi-compat/test/clm.test.mjs
```

See [qualification and reproduction](../../docs/pi-compatibility.md#qualification-and-reproduction).
