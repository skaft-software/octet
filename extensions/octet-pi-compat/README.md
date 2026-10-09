# Optional Pi factory compatibility

One optional Node/Bun process loads unchanged Pi extension factories after explicit
review. Import an enabled Pi 1.0.2 setup, or supply entrypoints manually. Rust
still owns octet's agent, sessions, approvals, terminal, keyboard focus and
shutdown. The installed-Pi fallback loads helpers inside this process, not a
second Pi CLI or agent loop. It is not an OS sandbox or universal Pi compatibility claim.
Full stable Pi 1.0.2 compatibility remains the target; this source preview does
not qualify that target, and new generic resource APIs do not substitute for Pi
parity.

## Local setup and configuration

Requires Node 22.19+ or Bun. Source dependency setup uses npm; release bundles
include the dependencies. Nothing executes during extension discovery, and
neither helper enables an extension or writes host configuration/trust:

```sh
node extensions/octet-pi-compat/setup.mjs
node extensions/octet-pi-compat/configure.mjs --reviewed --from-pi \
  --output /absolute/local-extensions/octet-pi-compat
# Add --provider-credentials only after explicitly reviewing that extra grant.
# Explicit activation is a separate host/user decision:
octet --extension-dir /absolute/local-extensions --enable-extension octet-pi-compat
```

`--from-pi` uses the installed Pi 1.0.2 resolver, probes each enabled factory on
octet's emulated path, then automatically tries installed Pi when loading fails
or a bounded source scan detects direct Pi tool execution, such as hashline's
image-read branch. Child-tool descriptors alone stay on the emulated path.
Setup prints each route or skip reason and records the Pi installation path.
Broken factories and unsupported schemas are diagnosed; constraints are never
stripped to make a tool load. Static registration collisions require explicit
ownership and fresh review, refusing a partial setup rather than silently dropping
other commands. Installed native manifests alone do not prove active ownership;
only explicit host-active tools conflict. Reviewed builtin tool replacements require
exact manifest grants and negotiated native `builtin_tool_overrides_v1` admission.
For a manual list, replace `--from-pi` with absolute entrypoint paths. Replace
`node` with `bun` to reuse Bun; configure records that executable. A load pass
does not qualify commands, hooks or UI behavior.

`--from-pi` also snapshots enabled Pi palettes (plus Pi dark/light) under `themes/`
with safe `pi-*` native selectors. Enabling this reviewed bridge contributes these
regular themes to `/theme` and makes Pi's selected palette the session startup
preference, even when host settings save `theme = "dark"`. `--theme`, `OCTET_THEME`,
and later `/theme` choices win for native appearance; Auto/Light/Dark/Cards/Still remain available. No
host or Pi settings are rewritten. `octet-config.toml` and printed theme arguments
are optional standalone alternatives, not extra steps for the bridge launch above.
Re-import to refresh these snapshots. Composer color uses Pi's configured thinking
level (unset: medium); dynamic per-model reasoning changes are not projected.
Terminal-dependent system/automatic themes are diagnosed rather than guessed.

The selected JSON snapshot also initializes the bridge's Pi Theme **before**
route probes, registration capture and factory loading. `ctx.ui.theme`, remote
editors/footers, helper callbacks and custom renderers share it; admitted installed-Pi
helpers use the same palette without calling Pi `initTheme` or starting a watcher.
Palette rebinding retires renderer caches and invalidates live component frames.
The native host currently supplies no theme selector/palette snapshot, so explicit
native `--theme` or later `/theme` changes cannot yet recolor Pi remote content:
those components keep the bridge-owned imported palette. No native palette service
or live host-theme synchronization is claimed.

Failing lifecycle observation callbacks are reported with their factory and event
while later callbacks continue. Permission/veto hooks, owner fences, cancellation
and failed host mutations still fail closed. This is isolation, not API parity.
Print and JSON output both use the native `print` context mode, without a UI.

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
The release catalog now includes this adapter with locked npm dependencies and
an empty factory configuration. Publication is still a separate gate. When using
an installed bundle, generate configuration in a separate directory rather than
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
an integrity claim over an extension's entire import graph. The shipped config
is empty and does not discover/import any Pi user installation.

octet 0.9.0 is pinned to **Pi 1.0.2**. Dependencies are the exact versions Pi
1.0.2 ships: jiti 2.7.0, `typebox` 1.3.27 and selected MIT Pi TUI 1.0.2 modules.
Imports resolve through the same module table as Pi 1.0.2's extension loader:
`@sinclair/typebox` maps onto the same TypeBox 1.x as `typebox`, the `pi-ai`
root and `pi-ai/compat` share one entry, and both `@earendil-works` and old
`@mariozechner` Pi imports alias to the same host facades. `pi-agent-core`,
`pi-ai/oauth` and `pi-ai/providers/all` come only from the installed Pi 1.0.2.
Setup uses `npm ci --ignore-scripts`, without global installs, asset downloads,
providers or model calls. There is no coding-agent npm dependency. jiti keeps
its default content-hashed transpile cache, as in Pi. Pure context helpers and
command-argument parsing/path normalization are adapted from Pi 1.0.2 (tag
`v1.0.2`, commit `cd32f7725fdbddbaecdff5b1e68491563394e0ca`) under its [MIT
license](LICENSE.pi); emulated helpers do not import Pi's session store or agent
runtime. The explicit installed-Pi route loads its managed 1.0.2 packages.

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
  unsupported *operation* still fails loudly. Octet-built data objects (observed
  assistant messages and their usage, native receipts, stream options) keep the
  opposite rule: an unavailable fact refuses instead of reading as absent.
- Pi 1.0.2 `config.ts` package helpers (`getPackageDir`, `getPackageJsonPath`,
  `getReadmePath`) resolved against the package that provides the
  `pi-coding-agent` module: this adapter package on the emulated path, the
  managed Pi release on the installed-Pi route. `docs/` and `examples/` get no
  helper because this package ships no such directories.
- Pi 1.0.2 `CURRENT_SESSION_VERSION` (3) and the `keybinding-hints` helpers
  `keyText`, `keyHint` and `rawKeyHint`, implemented from Pi's source against
  the pinned Pi TUI keybindings (host-supplied bindings on the installed route).
- The read-only keybinding managers supplied to custom UI components and returned
  by Pi TUI's `getKeybindings()` implement `matches`, `getKeys`,
  `getResolvedBindings` and Pi coding-agent's `getEffectiveConfig`; the latter
  two return the native host's complete effective snapshot, including overrides
  and reloads. Pi's remaining instance methods are
  explicit gaps on this injected facade: `getDefinition`, `getConflicts` and
  `getUserBindings` need definitions/override provenance not supplied by the host;
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
  subscribed native hooks. New command/shortcut/flag names remain startup-bound.
- Reviewed builtin overrides use exact `capabilities.builtin_tool_overrides`
  grants for captured names among `read`, `edit`, `write`, `bash` and
  `powershell`. Startup and late registrations must match the native initialize
  grant and negotiated feature; `bridge.json` cannot grant authority. The
  owner-scoped replacement keeps extension effects, scheduling, metering and
  unsafe replay classification, not the displaced builtin's privileges. Native
  retirement restores the builtin without retargeting frozen calls or replaying
  unsafe historical calls. First-party external tool conflicts remain refused.
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
  their visible labels as plain text. Other terminal escapes remain rejected. Rust paints cached lines and never calls JS synchronously.
- `setStatus` is local footer-provider metadata, not an assertion that every
  frontend displays ambient chrome. The compatibility palette is local, not a
  claim to reproduce octet's host theme; unknown roles are explicit errors.
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
API 0.4 lifecycle hooks, `tool_call`/`tool_result` to tool hooks, and
`input`/`before_agent_start`/`after_response` to available prompt/response hooks.
`turn_start`/`turn_end` use awaited native `model_turn_start`/`model_turn_end`
callbacks with a real session-leaf consumer, not whole-run `turn/started` or
`turn/settled` notifications. Whole-run observations remain `agent_start`/`agent_end`.
Message observations now carry `message`, accumulated text updates carry
`assistantMessageEvent`, and `agent_end.messages` contains the run-local observed
messages. Message and `agent_end` observers request native `before_prompt` and
`model_turn_end` hooks even without a Pi `turn_end` subscriber. These supply user
text and committed assistant messages with paired tool results before `turn_end`.
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
tool-result message boundaries arrive with the model-turn end hook rather than
each tool execution. Missing final usage/provider/stop metadata is not invented.
Other negotiated tool/compaction/model/dialog observations are dispatched in order.
Reading unavailable Pi fields or returning an unapplied transformation/veto fails
explicitly. Command argument arrays join with spaces; the wire cannot reconstruct
original shell quoting.

A synchronous reverse request that the host drops (it cancels the hook together
with the child request it issued) no longer takes the extension down. A dropped
*read-only* request (`tools/snapshot`, `composition/context`) is terminal
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
factory. Registration capture and adapter fixtures do not qualify CLM's seven
behavioral gates. Missing initial snapshots, unsupported projections/preparation
fields and checkpoint bounds remain open compatibility work.

**Rule A:** captured Pi breakages remain Octet-owned defects across the loader,
adapter, protocol and terminal integration. An explicit refusal is safer than fake
success, but is not a repair or closure. Qualification must match the original case,
candidate and security policy; historical passes do not qualify changed source.

## Native context, persistence and provider phases

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
native durable receipt. Hook writes require the actual one-use session-leaf grant;
only a known successor permits another append. No optimistic entry or Promise
flush counts as persistence. Cancellation after claim waits for the actual outcome;
transport loss means unknown outcome, terminal connection and no replay. Existing
16 KiB/depth16/nodes256 private-entry bounds remain; large CLM checkpoints can be
explicitly refused. Native entry mirrors translate lazily, preserve actual IDs,
parents and available timestamps, and expose only the initialized namespace's
private custom state. Unrepresentable mixed-message entries refuse; other native
markers retain their identities as `octet_native`, not fabricated Pi entries.

Negotiated `pipeline_hooks_v1` dispatches actual `before_provider_request`,
`before_provider_headers` and `after_provider_response`. These operate on encoded
JSON, header mutation/deletion patches, and real status/header arrival respectively,
not canonical context or inferred stream completion. Private callback exceptions
are redacted. Native code polices reserved headers and effects before send.

Provider credentials are withheld by default. Add `--provider-credentials` only
when the reviewed factories need Pi 1.0.2 `modelRegistry.getApiKeyAndHeaders`;
configuration records the separate `provider_credentials` capability, and the
host negotiates it only for that configured extension. Each call resolves exactly
the requested native provider/model. Returned secrets are available to the calling
factory only; they are never included in extension status or host diagnostics.
Request signers that cannot be represented as a Pi API key/header result are
refused. Reconfigure and explicitly enable the capability only after reviewing
all loaded factory code and its imports.

The original Anthropic-attribution provider remains blocked: builtin provider
inheritance is unsupported, and its direct HTTP callback requires OAuth-specific
request signing that Pi's raw key/header API cannot represent.

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
Unpriced usage remains readable; reading unavailable cost explicitly refuses.
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
[release status](../../docs/pi-compat-release-status.md), which also names the one
interactive acceptance that is red on the test machine. Non-resource factories
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
and complete owner for background owner-safe operations. Ordinary footer/header/
widget setters await all openings before their request settles. Close, resize,
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

The isolated working-candidate adapter and phase2 run is **900 tests: 879 passed, 0
failed, 21 optional skips**, exit 0, with the freshly built native editor test host.
This is synthetic/native-peer evidence, not existing-setup or attended RC qualification. The candidate-truth numbers are read from the shipped
`npm test` result; update them here when the adapter suite changes. Editor fixtures require the
compiled native test peer above (or `OCTET_NATIVE_EDITOR_TEST_HOST` pointing to it); missing
peers fail explicitly, never substitute a JS model. The immutable 193 upstream cases plus the
separate ownership guard pass **194/194 with zero skips**. See the
[corpus provenance and fixture boundary](test/pi-v1.0.2/README.md): native-service
coverage is distinct from actual App/adapter owner/admission/registry tests and
real-terminal qualification. The earlier two-batch adapter run covered 566 tests
with a disposable HOME: **539 passed, 27 skipped, zero failed**. The separate original-hashline/override run passed all
8 tests: installed-route selection, native-style reviewed grants, session-start
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
