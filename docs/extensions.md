# Executable extensions

Extensions run as subprocesses over JSON-RPC. The optional Pi adapter replicates
**Pi 1.0.2's public extension API** under this protocol; Pi defines its semantics.
The [27-row ledger](pi-extension-api.md) records implementation and real Rust-host
acceptance separately. Third-party package internals, the Pi CLI and child SDK
are not compatibility targets. Rust owns agent execution, sessions, policy,
persistence and the terminal; domain integrations remain optional.

Write new process extensions against **API `0.4`**, the current working-tree
version. It uses the feature-negotiated JSON-RPC wire retained from API `0.2`.
API `0.3` remains supported on its distinct canonical wire, and API `0.1` stays
frozen. What an extension can actually use depends on its exact manifest
selection, the host's offers and the frontend bindings, not on the API number
alone.

The [generated reference](extensions/API-0.4-REFERENCE.md) keeps the version
policy and the live canonical schema and models. The [feature-negotiated wire
reference](extensions/PROTOCOL-REFERENCE.md) keeps commands, hooks, status,
presentation, dynamic tools, artifacts, `agent_sessions`, approvals and the
other existing low-level services. Those contracts and their safety and
conformance tests stay live, but their breadth isn't an obligation to expose
every service in the coding product.

A manifest can declare options without running extension code. This is a
**manifest fragment, not a runnable extension**:

```toml
api_version = "0.4"

[contributes]
flags = [
  { name = "index-enabled", type = "boolean", default = true },
  { name = "index-root", type = "string", default = "." },
  { name = "index-limit", type = "integer", default = 500 },
]
```

The host resolves these options before startup and passes their values in
`initialize.params.flag_values` (the shared generated
[`InitializeFlagValue`](extensions/API-0.4-REFERENCE.md#initializeflagvalue)
shape). The rules are under [CLI flags](#api-04-cli-flags).

## Bounded authoring path

Start with the [build, install and maintenance quickstart](extensions/QUICKSTART.md)
for single-file Python/JS/TS tools, narrow Rust/C/C++ helpers, reviewed local
packages, explicit activation, lifecycle tests and whole-closure upgrades.

Use the [Python API `0.4` process
recipe](../sdk/python/README.md#minimal-api-04-tool) for a local tool. The SDK
handles framing, negotiated scheduling, cancellation and shutdown, so implement
only the contributions you need. It's a source recipe for **octet 0.8.2**: the
SDK is installed from source, not from a published registry.

An authoring smoke check covers discovery and explicit enablement, exact
negotiation, one real tool call, cooperative cancellation and clean shutdown.
Run the generated-contract, SDK and conformance checks for the wire you
selected.

<details>
<summary>The retained 0.3 example, and the compaction hook</summary>

The retained [ordinary-process API `0.3`
example](../examples/extensions/api-v03-minimal/README.md) uses only Python's
standard library and exercises canonical negotiation, an `echo` tool,
cancellation and shutdown. Its source manifest pins octet `0.8.0`, extension
version `0.1.0` and API `0.3`. Updating the host pin doesn't translate its wire
or establish publication, so don't retag it as `0.4`. The generated
`octet_extension.api_v03` and [TypeScript bindings](../sdk/typescript/README.md)
stay live contract implementations, not complete process runtimes. [Earlier
examples](../examples/README.md#legacy-extension-examples) keep their exact API
versions.

For a host-owned replacement of local compaction (not a model-callable tool),
see the source [octet-snap-compact
extension](../extensions/octet-snap-compact/README.md). API `0.4` offers its
`compaction_strategy` feature only to manifests declaring
`hooks = ["compaction_strategy"]`. The hook receives `{model_id, text}` in
`hook/run` batches and returns `{compaction_frames: [base64_png, ...]}`. The
host selects it only for vision models, keeps the checkpoint, and refuses
malformed, partial, oversized or over-budget renderings before committing. No
other API version negotiates this replacement hook.

</details>

## Tool composition

The optional first-party [octet-codemode](../extensions/octet-codemode/README.md)
bundle runs Pi's pinned offline QuickJS/WASM runtime (Node 22.19+). It adds
single-shot JavaScript batching/chaining/filtering of the host's existing tools,
not a new model, shell, filesystem or network authority. The guest cannot access
Node APIs; the trusted launcher remains an ordinary OS process subject to the
extension trust rules below.

Enable explicitly with `--enable-extension octet-codemode`. Startup flags
`--codemode-mode on|only` and `--codemode-inline-budget 0..16000` control
presentation, not policy. The default `on` preserves ordinary direct tools;
`only` keeps them in a separate frozen nested registry. Excluded/disabled tools
stay unavailable in either mode. `/codemode status` and the menu are read-only
help surfaces.

Embedders opt into the generic API `0.4` `tool_composition_v1` service through
`ExtensionRuntimeConfig.tool_composition`; it defaults false. The coding host
offers it to enabled trusted API `0.4` processes. The host binds dispatch only
to live model-tool parents, preserving schema validation, effects, approvals,
hooks, cancellation, session budgets, private durable receipts and branch-store
ancestry. Commands, hooks, stale/foreign parents and recursive compositions
cannot use it. See the [wire contract](extensions/PROTOCOL-REFERENCE.md#225-compositioncontext-compositioncall-compositionstore-api-04-feature-tool_composition_v1)
for declarations, limits and secure oversized JSON transport.

## Kernel boundary

The host owns model conversations, sessions and tool-result persistence,
permissions and approvals, process supervision and cleanup, and resource limits.
Extensions own domain behavior such as MCP, search, browser use, LSP or subagent
orchestration. A generic host service doesn't mean a capability package ships,
or that its domain joins the kernel.

Executable extensions run with your OS authority. Capability declarations are
consent metadata, **not a sandbox**. Message, queue, request, concurrency,
artifact, shutdown and process-tree bounds don't isolate CPU, memory, file
descriptors or PIDs. For full-access work, add OS-level isolation.

**Executable extensions are disabled by default, including installed bundles.**
Discovery never runs code. **Full access (`unsafe_host`, the default) trusts
selected extensions implicitly, but never enables them.** An explicitly enabled
extension can start without an extra authority grant, subject to the
`--no-process` and `--no-shell` capability gate and the existing source,
compatibility and integrity checks. This implicit authority is calculated for
the current policy, and never writes a persistent grant back into config.

`--safe-mode` (and other controlled policies) removes implicit host authority.
An **enabled extension with an explicit source-bound grant can start even in
safe mode**, and an ungranted one stays stopped. A grant lets its code run as a
host process with your OS permissions **outside the tool-effect broker**. Safe
mode still governs brokered tool effects, but it isn't an OS sandbox for the
extension, so use OS-level isolation for untrusted work. [Native-host protocol
1](sdk.md) is a separate embedding interface. It discovers extensions and never
starts them.

## Layout and discovery

Each direct child directory holds an `extension.toml`, and the directory name
must match the manifest `name`:

```text
.octet/extensions/workspace-index/extension.toml
~/.octet/extensions/workspace-index/extension.toml
```

Precedence is global, then trusted project, then explicit `--extension-dir`
directories in command-line order. Later wins by directory name. Project
resources are ignored until the workspace is trusted. On/off and host authority
stay separate. Full-access implicit authority applies to the selected, validated
source, without changing discovery precedence or enabling other installed
extensions. For example, with a reviewed bundle installed:

```sh
octet --enable-extension octet-web-search
```

`--enable-extension NAME` enables a selected extension for that invocation, and
`enabled_extensions = ["octet-web-search"]` persists activation in user config.
`--trust-extension NAME` grants host authority to the selected source for this
invocation, even in safe mode, and doesn't turn the extension on. An explicit
`--extension-dir` grants host authority for that invocation without an extra
flag, but doesn't enable the extension. `trusted_extensions` is the existing
user-config list of **persistent host authority grants**: a bare name applies
only under `~/.octet/extensions`, while `NAME@/absolute/path/extension.toml`
grants one exact project or other source. A same-named project shadow never
inherits a global grant, and the workspace must be trusted first. Revoking an
explicit grant doesn't override full-access implicit authority.

A trusted project config can suggest `enabled_extensions` but can't create a
persistent trust grant. Persistent trust comes from user config or
`OCTET_TRUSTED_EXTENSIONS`, never from that project suggestion.

`/extensions status` shows what's discovered, enabled, trusted and running.
`/extensions` shows On/off and Host authority per row and offers Grant and
Revoke. Enabling an ungranted extension under safe mode prompts for host
authority, and an off extension never starts, even with authority. Config
examples, manifest reads, diagnostics and resolver APIs are in the [discovery
and trust reference](extensions/legacy-authoring.md#layout-and-discovery).

## Manifest

Select `api_version = "0.4"` exactly. An extension's own `version` doesn't
select the wire. Current source uses `octet_version`, `requires_octet`,
`OCTET_*` and `octet_extension`, with no aliases for earlier first-party wire
names or imports. The local host, SDK source packages and six executable
bundles have distribution version **0.8.2**. That doesn't select an extension
API or publish SDK registries. Catalog installation needs version-matched
published assets: see [installation](installation.md) and the [release
notes](releases/v0.8.2.md).

Declare the entrypoint and the tools you actually supply. Return the complete
tool and command catalogs and the selected protocol features from
[`initialize`](extensions/PROTOCOL-REFERENCE.md#11-initialize), and select only
features the host actually offers. The canonical API `0.3` `contract`
negotiation is different, so don't copy it into a `0.4` process. The [retained
manifest reference](extensions/legacy-authoring.md#manifest) keeps earlier
examples and shared host operations, not a license to retag their
implementations.

`requires_octet` is optional for an unpackaged local extension, enforced when
present, and mandatory (as an exact running version) for an installed bundle.
Installing never enables, records a trust grant, starts or runs setup code.
Full-access trust is a runtime policy, not an installer side effect. See [bundle
validation and
commands](extensions/legacy-authoring.md#installable-extension-bundles).
Published-catalog examples need matching published assets, so use a reviewed
source or local archive when matching publication hasn't been verified.

<a id="api-03-cli-flags"></a>

### API `0.4` CLI flags

Each `[contributes].flags` entry needs a globally visible `name`, an exact
`type` and a typed `default`. Types are `boolean`, `string` and `integer`. A
`description` is optional short plain text.

- Booleans accept `--index-enabled` and `--no-index-enabled`. Strings and
  integers take one value: `--index-root src`, `--index-limit 1000`.
- Integers are signed portable JSON integers. Names are at most 64 bytes: a
  lowercase letter first, then lowercase letters, digits or hyphens. Defaults
  and supplied strings are bounded. Each extension can declare at most 64 flags.
- Unknown fields, duplicates, invalid identifiers, unsupported types, wrong
  default types and oversized values fail manifest validation.
- The host reads only validated metadata and never starts or imports an
  extension to find options. Flags register only for selected, enabled, trusted
  extensions. A collision with a built-in option, another selected extension or
  a generated boolean inverse rejects the invocation before startup.
- Flags show in `octet --help` for ordinary no-subcommand startup.
  Authentication, package, migration and other early-exit paths keep their
  static parsing.
- All values, including omitted defaults, go to `initialize.params.flag_values`
  as a sorted `{name, value}` list.

API `0.1` manifests can't declare these flags. Supported later manifests keep
their selected initialization wires. Declaring a flag doesn't run third-party
code or imply Pi `registerFlag` compatibility.

## Transport contract

API `0.4` uses UTF-8 JSON-RPC, one compact object per line, with stdout reserved
for protocol and bounded diagnostics on stderr. Initialization negotiates a
`protocol` feature list and a concurrency bound, not the canonical `contract`
object. See the [wire reference](extensions/PROTOCOL-REFERENCE.md) for exact
messages, feature dependencies, request and owner correlation, and limits.
Dynamic tools used by MCP, `agent_sessions` used by subagents, and command,
status and presentation surfaces stay available subject to their existing host
gates. A documented approval or secret broker isn't automatically offered by a
host.

### Typed values, native resources and immutable bulk (API `0.4`)

The [Python](../sdk/python/README.md) and [Rust](../sdk/rust/README.md) SDKs
support generated typed JSON inputs/outputs, explicit model-facing text and
validated diagnostics. Native objects opt into `resource_refs_v1` plus
`operation_descriptors_v1`: the extension keeps the object while the host
mediates opaque ResourceRefs, exact nominal types, exclusive admission and
explicit cleanup. Ordinary tools, including Pi tools, need no annotations.

Hosts may opt into bounded applicable-operation discovery and lazy model-schema
projection without changing their full registered/active catalogs. Configured
`bulk_objects_v1` storage transports numerical bytes through immutable verified
`local-file.v1` snapshots; model results contain only BlobRefs and summaries.
ResourceRefs do not survive owner/generation/host retirement. Durable blobs
require explicit host retention/recovery; transport tickets and locators are
never durable domain data.

See the [wire methods](extensions/PROTOCOL-REFERENCE.md#226-native-resource-lifecycle-api-04),
[approved contract](design/extension-values-v1.md) and
[required conformance cases](design/extension-values-v1-conformance.md).
Implementation is not a full conformance or Pi compatibility claim; actual
SPICE execution additionally requires `ngspice`. These services do not sandbox
a trusted subprocess's operating-system access.

### Tool prompt metadata (API `0.4`)

The optional `tool_prompt_metadata_v1` feature carries presentation metadata in
initialized and dynamically registered tool definitions:

```json
{"name":"inspect","description":"Inspect a project","parameters":{"type":"object"},"prompt_snippet":"Inspect project structure","prompt_guidelines":["Report only verified findings"]}
```

`prompt_snippet` is optional; `prompt_guidelines` defaults to an empty array.
Each string is limited to 1,024 UTF-8 bytes, with at most 16 guidelines per tool.
Newline and tab are allowed; other C0/C1 controls are rejected. Metadata also
counts toward the existing aggregate catalog byte budget. A snippet or nonempty
guidelines array requires this negotiated feature on API `0.4`; older wires are
unchanged.

The Agent includes explicit metadata only from its model-visible, policy-filtered
active tool snapshot. Guidelines work without a snippet. Empty/whitespace-only
contributions add nothing. The section retains the Agent's existing byte/count
bounds and is frozen for that run, just like the system prefix; changes take
effect on the next run. Tool-free runs omit it. Ordinary tools without metadata
retain their existing behavior, including the default-disabled legacy built-in
prompt section. Embedders assembling their own prompts can read the owned
`Tool::prompt_metadata()` contribution. Metadata never changes effects,
authorization, schemas, handlers, or active-tool selection.

The Pi adapter maps `promptSnippet`/`promptGuidelines` to these fields. This is
one binding, not a full Pi compatibility claim.

### Autocomplete edits (API `0.4`)

The optional `autocomplete_edit_v1` profile requires negotiated `autocomplete`
and uses the existing `ui/autocomplete/complete` RPC, not a new editor service.
Choices may include `replace_after_bytes` (additional original bytes after the
cursor to replace, absent = zero, at most 262,144) and `cursor_offset_bytes`
(cursor within the inserted value, absent = its UTF-8 byte length). Both are
optional `u32` fields; explicit null or any unnegotiated presence, including
zero, is invalid. Prefix/value may contain LF, TAB and CR under this profile;
labels/descriptions remain control-free. Each string stays within 1,024 UTF-8
bytes and each response within 32 items.

The host validates the exact snapshot, UTF-8 edit range and resulting editor
budget at response, display and explicit acceptance. Live provider instance,
generation, session/resource owner and text/cursor/revision/focus fences prevent
stale edits or fallback into a new draft. The native editor also requires
representable grapheme boundaries: it refuses an incompatible edit/cursor
rather than silently moving the requested cursor. See the [wire profile and
example](extensions/PROTOCOL-REFERENCE.md#autocomplete-edit-v1). This documents
the native contract, not Pi adapter support, full parity or completed feature
qualification.

### Retained canonical API `0.3` wire

Use UTF-8 canonical JSON followed by exactly one LF, with stdout for protocol
only and bounded diagnostics on stderr. API `0.3` framing is stricter than
ordinary JSON lines: duplicate keys, unknown envelope fields, noncanonical
whitespace and escapes, malformed surrogates, nonportable numbers and excessive
depth are rejected before dispatch. The frame limit excludes the LF. A frame at
the limit is accepted, and one byte over ends the stream. After initialization,
the negotiated limits replace the offered ones, in both directions, atomically.

Build against these reference sections, not a legacy JSON example:

- [Canonical
  envelopes](extensions/API-0.4-REFERENCE.md#canonical-framing-and-json-rpc-envelopes)
  and [all bounds](extensions/API-0.4-REFERENCE.md#bounds).
- [Host offer](extensions/API-0.4-REFERENCE.md#contractoffer),
  [selection](extensions/API-0.4-REFERENCE.md#contractselection) and
  [initialization fields](extensions/API-0.4-REFERENCE.md#initializerequest).
- [Capabilities](extensions/API-0.4-REFERENCE.md#capabilities), [method
  directions and terminal
  semantics](extensions/API-0.4-REFERENCE.md#methods-and-terminal-semantics) and
  [exact error code/message pairs](extensions/API-0.4-REFERENCE.md#errors).
- [Tool arguments](extensions/API-0.4-REFERENCE.md#toolcallparams),
  [results](extensions/API-0.4-REFERENCE.md#toolcallresult),
  [cancellation](extensions/API-0.4-REFERENCE.md#cancelrequestparams) and
  [shutdown](extensions/API-0.4-REFERENCE.md#shutdownparams).

<details>
<summary>What's deferred on the 0.3 wire</summary>

On this canonical wire, only foundation methods and capabilities can be
negotiated, and optional services can be omitted when the host can't bind them
safely. `dynamic_tools`, `tools/register`, `tools/unregister` and
`context/collect` are deferred **on API `0.3`**, not removed from the
feature-negotiated API `0.2` and `0.4` wire. `ContentPart` has foundation text,
with image and audio variants deferred. Legacy artifact, progress, command,
presentation, secret and child-session methods aren't implicit API `0.3`
capabilities.

The schema also defines optional migration, provider catalog, auth and stream,
and session-lifecycle services. A declaration in the schema doesn't prove a
particular product host offers the service. An ambiguous provider acceptance on
these extension services never allows automatic replay, and the separate
host-qualified Codex local-function inference exception doesn't qualify an
extension provider or its effects. Authorization uses opaque host-policy leases,
not credentials or URLs in protocol fields. The generated models and the host
offer are exact.

</details>

## Lifecycle and reload

The host stays responsible for cleanup and persistence. Cancellation is
cooperative, not rollback, and ambiguous unsafe work isn't replayed. Keep
terminal disposition and generation ownership. Don't infer success from a lost
connection. The [host lifecycle
reference](extensions/legacy-authoring.md#lifecycle-and-reload) covers
candidate-first reload, bounded drain, shutdown and supervised restart. Its
legacy observational methods aren't API `0.3` methods.

### Provider-retry observations and advice

The Rust `ProviderRetryKind` and the retained API `0.2` and `0.4` provider-retry
hook wire add `InterruptedInference` / `interrupted_inference` and
`WaitingForNetwork` / `waiting_for_network`. Hooks can veto only the proposed
retry for their operation, or add bounded delay. They can't authorize
unqualified replay, expand budgets, shorten `Retry-After` or override
cancellation. Extension API versions are unchanged.

<details>
<summary>Hook context, operations and events</summary>

`max_attempts` is optional (`Option<usize>` in the Rust hook context, JSON
`null` for sustained pre-send waiting), not a made-up finite denominator.
`ProviderRetryContext.operation: Option<ProviderOperation>` is `None` for main
sampling, serialized as an explicit `"operation": null` (not omitted). Auxiliary
hooks receive `"operation": "local_compaction"`, `"native_compaction"` or
`"terminal_gate"`, including manual native compaction. These hooks can veto only
the proposed retry for that operation, or add delay (cumulative additional delay
capped at five seconds). They don't roll back an unrelated main answer or change
the original failure classification. The existing `before_generation` and
`stream_start` kinds remain. Advice can decline a host-authorized retry or add
bounded delay, but can't authorize unqualified replay, expand budgets, shorten
`Retry-After` or override cancellation. These aren't new API `0.3` request-path
hooks. The session schema version is also unchanged, but the additive
`usage_uncertainty` record evolves its record contract.

`AgentEvent::ProviderWaitingForNetwork` is live recovery telemetry, not
assistant content or run completion. Serve keeps its run owner alive during the
wait without adding a durable status item. The separate unit
`AgentEvent::ProviderUsageUncertain` reflects durable session accounting
uncertainty: Serve keeps it through completion and prefixes completion-review
summaries with a warning that numeric usage and cost values are known subtotals.
Neither event is assistant output. See the [recovery
boundary](tools.md#recovery-and-security).

</details>

### Cache-warming decision advice (API 0.4)

API `0.4` manifests may declare `hooks = ["cache_warming_decision"]` and
negotiate the matching optional feature. It is offered only to declarations on
API `0.4`, not on frozen `0.1`, retained `0.2`, or canonical `0.3`.

Before every due refresh, `hook/run` supplies a content-free payload:

```json
{"decision":{"phase":"idle","warm_cost_microdollars":100,"miss_cost_microdollars":1000,"continuation_probability":0.15,"expected_savings_microdollars":50,"economics_available":true,"action":"stop"},"model":"cache-model"}
```

`phase` is `streaming` or `idle`; costs and signed expected savings are in
microdollars. The separate execution context carries the host-issued resource
owner, instance and process-generation fence. No prompt, credentials, provider
endpoint, or mutable session is disclosed. Return
`{"cache_warming_decision": "warm"}` or `{"cache_warming_decision": "stop"}`;
an absent or null field is no opinion. Other hook dispositions, prompt contributions and notifications do
not control a refresh.

Hooks run in registration order; the **last returned action wins**. Invalid
responses, remote failures, stale generations and no opinion leave the preceding
host/hook decision unchanged. The process adapter caps each wait at 200 ms; the
host also enforces one aggregate hook budget capped by the original refresh
deadline. Advice cannot enable an off, unsupported or ineligible refresh,
expand any spending/attempt budget, override cancellation, or authorize provider
replay. Keep handlers fast and local: the host alone performs provider I/O.

Native Rust extensions register `CacheWarmingDecisionHook` through
`ExtensionHost::cache_warming_decision_hook`; the read-only
`CacheWarmingDecisionContext` contains the typed `decision`, `model` and durable
`resource_owner`. The Python SDK offers `@ext.cache_warming_decision`, returning
`"warm"`, `"stop"` or `None`, plus typed payload/result models. See the
[SDK recipe](../sdk/python/README.md#cache-warming-decision-advice-api-04) and
[local example](../examples/extensions/cache-warming/README.md). This is bounded
advice, not Pi ABI compatibility or ownership of the cache scheduler.

### Declared API `0.3` session hooks

The optional cleanup hooks are an all-or-nothing pair:

```toml
api_version = "0.3"

[contributes]
hooks = ["session_start", "session_end"]
```

Under canonical API `0.3` the hooks are declared as a pair, and a partial pair
is rejected. Initialization must select both optional `lifecycle_events` and
`hook/run`, or neither. The host sends `session_start` when the product
activates its durable session owner, and `session_end` when that binding
settles.

<details>
<summary>Hook rules, ordering and the separate session service</summary>

These hooks are distinct from the optional `session_lifecycle` service. One
`hook/run` with `hook: "session_start"` is delivered when the coding product
activates its durable session owner, and one with `hook: "session_end"` when
that binding settles.
[`SessionBinding`](extensions/API-0.4-REFERENCE.md#sessionbinding) holds only an
opaque SHA-256-derived owner key, a host-made instance fence and the process
generation. `SessionEnd` adds an outcome, a `shutdown`, `reload`, `crash` or
`cancelled` reason, and a duration in milliseconds. Neither exposes a session
path, a mutable `Session`, a prompt, a host-state snapshot or request-path
context.

Ownership is recorded before start to prevent duplicate pairs. Each dispatch is
capped at 250 ms. Malformed results, timeouts and remote errors become bounded
diagnostics. A valid `deny` or `defer` is recorded but can't veto the host's
lifecycle ownership. Settlement is idempotent and happens before child shutdown.
On an accepted reload, the old generation gets its `interrupted`/`reload` end
before the replacement's start. After a detected crash, the replacement first
gets the old `interrupted`/`crash` end, with the old binding generation, before
its own start. These hooks stay out of the prompt and tool hot path, and they
aren't the legacy `session/started` or `session/settled` observations.

The separate optional `session_lifecycle` capability is offered only when the
interactive product has a safely bound active-session driver. `session/create`
and `session/fork` return durable IDs without switching. `session/switch` takes
an existing workspace session ID, and `session/reload` rereads the active
durable session at an idle boundary. See [the generated request
models](extensions/API-0.4-REFERENCE.md#sessioncreateparams).

</details>

## Retained reference topics

[Retained feature-negotiated hook enrichments](extensions/HOOK-ENRICHMENT.md)
document bounded progress decoration, namespaced pre-persistence metadata and
PostMutation rescans, including current product integration limits.

These anchors keep links from the former combined guide working. The linked
maintenance references keep earlier wire examples and the services API `0.4`
reuses. Version-specific payloads and host-availability limits still apply.

- <a id="runtime-lifecycle-and-sharing"></a>[Runtime lifecycle and
  sharing](extensions/legacy-authoring.md#runtime-lifecycle-and-sharing)
- <a id="live-tool-catalogs"></a>[Live tool
  catalogs](extensions/legacy-authoring.md#live-tool-catalogs)
- <a id="semantic-presentation-snapshots"></a>[Semantic
  presentation](extensions/legacy-authoring.md#semantic-presentation-snapshots)
- <a id="child-model-session-service"></a>[Child model-session
  service](extensions/legacy-authoring.md#child-model-session-service)
- <a id="optional-kernel-services"></a>[Brokers, correlation, input, and
  approvals](extensions/legacy-authoring.md#optional-kernel-services)
- <a id="python-sdk"></a>[Python SDK status](../sdk/python/README.md) and
  [legacy runtime](../sdk/python/legacy-runtime.md)
- <a id="capability-boundaries"></a>[Capability ownership
  boundaries](extensions/legacy-authoring.md#capability-boundaries)
- <a id="installable-extension-bundles"></a>[Bundle installation, update,
  removal, and
  validation](extensions/legacy-authoring.md#installable-extension-bundles)
- <a id="first-party-application-packages"></a>[Separate Serve application
  packages](extensions/legacy-authoring.md#first-party-application-packages)

See also the [legacy wire reference](extensions/PROTOCOL-REFERENCE.md) and
[project tracking](https://github.com/orgs/skaft-software/projects/5).
