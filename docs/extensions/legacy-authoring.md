# Legacy extension authoring and host operations

This is a **maintenance reference for API `0.1` and `0.2` implementations**,
retained from the former combined extension guide. It is not a new-extension
quickstart. New authoring uses [API `0.3`](../extensions.md); the
[generated contract](API-0.3-REFERENCE.md) alone defines that wire. Shared host
operations below do not upgrade legacy methods or SDKs.

**Identity boundary:** octet 0.7.5 source uses only octet first-party names,
including `octet_version`, `requires_octet`, `OCTET_*`, and `octet_extension`.
Retained API numbers do not imply aliases for old Ygg wire names or imports.
The source SDK distributions and four official executable bundles are version
`0.7.5`; the Pi compatibility bridge and independent examples keep their own
versions. This is not SDK registry publication; see
[installation and availability](../installation.md).

The [legacy protocol reference](PROTOCOL-REFERENCE.md) retains complete API
`0.1`/`0.2` method, request/response, type, and lifecycle detail. Process
extensions use JSON-RPC 2.0, one compact JSON object per line, over stdin/stdout.
They may be written in any language that can read and write JSON lines, alongside
native Rust `Extension` implementations.

## Kernel boundary

The host owns services that an extension needs in order to exist safely:

- starting, supervising, stopping, and force-killing extension process groups;
- transporting bounded JSON-RPC messages;
- running model conversations;
- persisting sessions, tool calls, and tool results;
- enforcing user permissions and approvals; and
- resource-limit policy for memory, messages, concurrent work, artifacts, and
  child processes.

Product capabilities belong in subprocess extensions. MCP bridging, web search,
browser use, computer use, memory, LSP, subagent orchestration, and caffeinate
are domain behavior, not kernel prerequisites. Artifact ingestion and child
model-session creation are generic host services. This architectural boundary
is not a claim that a first-party package for every capability ships.

Current runtime enforcement covers message, queue, request, concurrency,
artifact, shutdown, and process-tree cleanup bounds. OS CPU/RSS/FD/PID isolation
is not implemented: trusted extensions retain the current user's OS authority.
For example, an `octet-mcp` process can supervise MCP servers and publish their
changing catalogs without teaching the host MCP:

```text
octet <- JSON-RPC -> octet-mcp <- MCP -> Ableton MCP
```

The extra local hop preserves language neutrality, replaceability, failure
isolation, and a host that does not grow a manager for every external protocol.
For capability ownership, trust propagation, and non-goals, see
[capability boundaries](#capability-boundaries) and
[project tracking](https://github.com/orgs/skaft-software/projects/5).

Manifest-selected versions are exact:

- API `0.1` is frozen for existing trusted, bounded, text-oriented extensions.
  Its initialization wire remains unchanged. It does not inherit API `0.2`
  cancellation, progress, structured/media retention, correlation, or terminal
  lifecycle guarantees. Optional `metadata` is accepted but discarded by the
  native subprocess adapter.
- API `0.2` is the supported stateful legacy foundation. Cooperative
  cancellation and typed content are required features; scoped progress,
  artifacts, lifecycle observations, policy intents, and live tool catalogs are
  optional. Child model sessions, single-use approvals, and owner-scoped secrets
  are conditional; parent-correlated ephemeral input is part of the base wire.
- API `0.3` is current, canonical, and schema-generated, not an implicit upgrade
  of either legacy wire. Its bounds, methods, errors, capabilities, and
  availability belong to the [generated reference](API-0.3-REFERENCE.md).
  Its [CLI values and paired session hooks](../extensions.md#api-03-cli-flags)
  do not exist in the legacy APIs.

No version adds an OS sandbox or implicit access beyond negotiated host
capabilities. Pi migration is capability-oriented, not Pi's in-process ABI:
`octet migrate pi --dry-run` inventories resources without executing them;
`octet pi install` creates an inert wrapper for the bounded `octet-pi-compat`
subset. See [Pi migration](../pi-migration.md).

Discovery is available under every effect policy; executable extensions remain
disabled until explicitly enabled. In the coding product, startup requires
enablement, trust, the `unsafe_host` effect-policy floor, and independent
process permissions. Default full access (`unsafe_host`) implicitly trusts the
selected extension without persisting a grant or enabling it. `--safe-mode`
does not inherit that implicit trust and never starts an executable extension,
even with explicit trust grants or enabled process/shell flags.
`/extensions status` reports the blocked startup. Use full-access mode only
inside separate OS-level isolation. Capability declarations are visible consent
metadata, not an OS sandbox.

## Layout and discovery

Each direct child directory contains one `extension.toml`:

```text
.octet/extensions/git-tools/extension.toml
~/.octet/extensions/git-tools/extension.toml
```

Precedence is global, then trusted project, then explicit directories in
command-line order; later definitions win by directory name. Project extensions
are ignored until the workspace is trusted. Discovery never executes code;
enablement remains explicit. Coding-product full access implicitly trusts the
selected extension; optional explicit grants remain bound to the selected
manifest name and source. The direct child name must exactly match the manifest
`name`; aliases are rejected with a diagnostic.

Repeatable one-shot options select and activate an existing reviewed extension:

```console
octet \
    --extension-dir ./my-extensions \
    --enable-extension hello-world
```

Or persist activation in user config:

```toml
# Unsafe: full-access mode is intended only inside separate OS-level isolation.
# Use --safe-mode when approval is required.
enabled_extensions = ["hello-world"]
```

Optional explicit trust grants do not enable an extension. A bare persistent
trust name applies only under `~/.octet/extensions`, never to a same-named
project or explicit extension. To record an explicit grant for those sources,
use an exact absolute manifest path:

```toml
enabled_extensions = ["git-tools"]
trusted_extensions = [
  "git-tools@/absolute/project/.octet/extensions/git-tools/extension.toml",
]
```

`--trust-extension git-tools` optionally grants explicit trust to the
currently selected source only for that process invocation; it is never
written back as a persistent name grant. A trusted project config may suggest
`enabled_extensions`, but cannot create explicit executable-trust grants.
Persistent grants come from user config or environment
(`OCTET_TRUSTED_EXTENSIONS`); one-shot grants come from `--trust-extension`.
Coding-product full access supplies implicit trust independently of those
grants; `--safe-mode` does not inherit it and keeps executable extensions
stopped even with explicit grants.

The agent crate exposes `discover_extension_manifests` for direct-child layouts
and `ExtensionCatalog::load_resolved` for already resolved manifest paths in
authoritative precedence order. The latter retains diagnostics instead of making
one bad extension disable the catalog. Manifest reads are bounded; selected files
must be regular, non-symlink files. Malformed or shadowed resources produce
inspectable diagnostics without preventing core startup.

## Manifest

This retained illustration is **API `0.2`**, not a current-API template:

```toml
name = "git-tools"
version = "0.2.0"
api_version = "0.2"
# Required for an installable bundle; optional for an unpackaged local copy.
requires_octet = "=0.7.5"
description = "Small local git helpers"

[entrypoint]
command = "git-tools"
args = ["--stdio"]

[capabilities]
filesystem = "workspace" # none, workspace, or unrestricted
process = true
network = false
secrets = ["git.provider_token"] # exact logical names; empty by default
environment = [] # API 0.2 reviewed broker names only; e.g. SSH_AUTH_SOCK

[contributes]
tools = ["git_status"]
commands = ["checkpoint"]
hooks = ["after_tool_call"]
ui = ["status"] # status, header, or footer
context = true
tool_renderers = ["git_status"]
notifications = true
confirmations = true
presentation = true # API 0.2 frontend-neutral activity/list/tree/detail snapshots

# Optional. Omitting this preserves the legacy isolated resident process.
[runtime]
lifecycle = "workspace_service"
sharing = "workspace"
```

### Runtime lifecycle and sharing

Discovery builds a bounded static catalog without launching entrypoints. The
runtime manager activates only profiles admitted by enablement, trust,
effect-policy, and process-policy gates. `[runtime]` defaults to
`lifecycle = "legacy_resident"`, `sharing = "isolated"`. Valid lifecycles are
`legacy_resident`, `lazy_resident`, `oneshot`, `session`, `workspace_service`,
`always`, and `pi_aggregate`.

- `legacy_resident` and `session` start with an admitted session binding;
  `session` and every isolated resident stop when it is released.
- `lazy_resident` starts only after explicit host activation. It can be isolated
  or explicitly workspace shared.
- `oneshot` is isolated and stops when its admitted operation settles.
- `workspace_service`, `always`, and `pi_aggregate` require
  `sharing = "workspace"`; their manager-owned process may survive a compatible
  App/session rebuild until host shutdown or a catalog change.

Workspace sharing is never inferred from a name or language. It requires a
canonical workspace, explicit host trust partition, manifest opt-in, matching
content digest, and directly verified local entrypoint. Ordinary and Serve hosts
use disjoint trust partitions; Serve further separates project and authority
profiles. PATH-only commands are permitted for isolated legacy compatibility,
not sharing. API `0.1` lacks resource-owner fences and is always isolated.
Source/catalog changes retire shared runtimes fail-closed. Runtime status and
`resource_exhausted` expose extension/digest provenance, not workspace paths,
trust inputs, child stderr, or secrets.

Bare commands resolve beside the manifest, then through `PATH`. Arguments pass
directly without a shell; the child's working directory is the active workspace.
The host supplies `OCTET_EXTENSION_API_VERSION`, `OCTET_EXTENSION_NAME`,
`OCTET_EXTENSION_DIR`, `OCTET_EXTENSION_MANIFEST`, `OCTET_WORKSPACE`, and
`OCTET_EXTENSION_SCRATCH` for host-verified artifact publication each generation.
Leave `api_version = "0.1"` to retain the frozen wire. Semantic `presentation`
is API `0.2`-only and rejected on a frozen `0.1` manifest.

`[capabilities].secrets` is a duplicate-free allowlist, not launch-environment
injection. Names are at most 64 ASCII bytes: letter/underscore first, then
letters, digits, underscore, hyphen, or dot. A non-empty list makes `secrets`
eligible for negotiation only with a configured host broker; it never makes an
undeclared name readable.

`[capabilities].environment` is a narrow API `0.2` ambient broker, not arbitrary
inheritance. The only reviewed name is `SSH_AUTH_SOCK`, for an extension using
an already configured agent without collecting credentials. The default
subprocess environment excludes it. It is copied only when explicitly declared
and present, never persisted or logged by the host. This grants signing authority
to the trusted process; use the same full-access/OS-isolation boundary. Unknown
names and API `0.1` declarations are rejected.

### API `0.3` CLI flags

This is not a legacy feature. The complete declaration, parsing, default,
collision, and initialization rules moved to
[API `0.3` CLI flags](../extensions.md#api-03-cli-flags). API `0.1`/`0.2`
initialization fields are unchanged.

## Transport contract

Stdout is protocol-only; human diagnostics go to stderr, drained as bounded
events. Default maximum JSON line is 1 MiB, in-flight cap is 64, and ordinary
requests time out after 30 seconds. A bounded dedicated writer serializes
complete frames; API `0.2` additionally schedules behind negotiated concurrency.
See [transport defaults](PROTOCOL-REFERENCE.md#transport-defaults) for every
queue, timeout, tombstone, manifest-read, and shutdown bound.

Every request/response uses a JSON-RPC 2.0 envelope. The first host request is
`initialize`, containing API/octet versions, extension identity/source,
workspace, capability/contribution declarations, and inspectable
session/model/reasoning/active-skill state. The response must select the same API
and supply complete tool and command definitions. Without negotiated discovery,
names must exactly match the manifest. Negotiated `dynamic_tools` makes the
initialize tool list authoritative epoch zero; `runtime_commands` permits a
compatibility runtime's generation-fixed command catalog. See
[exact initialization examples and validation](PROTOCOL-REFERENCE.md#11-initialize).

`runtime_commands` does not authorize post-initialize command registration. A
reload must return the same command definitions or require a product rebuild.
For enabled first-party `octet-subagents`, the host requires
`delegation_telemetry_v1` whenever it offers `agent_sessions`; an older bundle
is rejected with an actionable reinstall diagnostic. The native schema is
`octet.delegation.telemetry.v1`, shown with manifest and bundle digests by
`/extensions status`.

The child-session service creates an extension-only V2 delegation manager after
successful negotiation. It is available independently of the parent's reasoning
effort; Ultra additionally requires matching provider V2 metadata. It is not
offered when the observing extension is unavailable. Conditional `approvals`
requires configured issuance and `policy_intents`; conditional `secrets`
requires a broker and non-empty manifest allowlist. Never return a feature the
host did not offer. Missing required/unknown/duplicate features, version mismatch,
or zero concurrency rejects initialization; the host caps accepted concurrency.
Omitting or emptying a negotiated lifecycle subscription selects all six events.
API `0.1` omits `protocol` and forbids `output_schema`.

All legacy host-to-extension methods, payloads, and results are retained in
[section 1 of the wire reference](PROTOCOL-REFERENCE.md#1-host-to-extension-methods):
`tool/call`, `command/execute`, `hook/run`, `context/collect`, `status/collect`,
`tool/render`, and `shutdown`, typed in
`octet_agent::extension_process::methods`. API `0.2` tools require compact text;
image `alt` and audio `transcript` are optional. Media requires a verified
same-owner, same-generation artifact with exact MIME. `structured_content` is
required exactly when `output_schema` is declared and is retained with bounded
metadata as non-model-visible native details, subject to explicit host policy
for structured content. See [tool results and limits](PROTOCOL-REFERENCE.md#12-toolcall).

All reverse messages remain documented in
[section 2](PROTOCOL-REFERENCE.md#2-extension-to-host-messages): `notification`,
`confirmation/request`, `context/contribution`, `status/contribution`, and API
`0.2` `presentation/update`, `$/progress`, `input/request`, `artifact/publish`,
`policy/evaluate`, `secret/get`, `tools/register`, `tools/unregister`,
`agent/spawn`, `agent/message`, `agent/follow_up`, `agent/list`, `agent/wait`,
and `agent/interrupt`. These names are not API `0.3` aliases.

### Live tool catalogs

API `0.2` `dynamic_tools` supports transactional `tools/register` and
`tools/unregister`. The initialize catalog is epoch `0`, the only deterministic
first-model-request catalog. Put turn-one tools in the initialize response;
post-initialize registration is not guaranteed to enter the first request.
Native API `0.2` with negotiated `dynamic_tools` accepts an authoritative initialize
catalog that need not match manifest tools. Static catalogs and Python `Extension`
bootstrap decorators must match the manifest; Python checks before negotiation.
Mutations begin only after initialization and host registration.

The exact mutation payloads, 256-tool bounds, authoritative accepted-name list,
monotonic `revision`, conflict errors, policy filtering, publication-before-ack
failure handling, and reload rules remain in
[`tools/register`](PROTOCOL-REFERENCE.md#211-toolsregister-api-02) and
[`tools/unregister`](PROTOCOL-REFERENCE.md#212-toolsunregister-api-02).
Malformed/conflicting requests with parseable IDs return `-32602` without changing
the previous catalog. Undeliverable acknowledgements after publication remove the
group and terminate the generation rather than allowing catalogs to diverge.

Each model request freezes schemas and implementations. A mutation appears at
the next model-request boundary, never halfway through a provider request; no
implicit startup-quiescence wait exists. Calls carry `catalog_revision` for the
handler version the model saw even if replaced or removed later. Extensions
retain bounded historical snapshots; Python retains eight and rejects unknown
or retired epochs with `-32602`. Reload starts a new epoch `0`.

### Semantic presentation snapshots

An API `0.2` manifest declaring `presentation = true` may publish complete
`presentation/update` notifications. They are frontend-neutral atomic state
replacements, not rendering code or model results. The
[exact snapshot example and schema](PROTOCOL-REFERENCE.md#25-presentationupdate-api-02)
retain activity/list/tree/detail, action, metrics, state, reference, ownership,
rate, and byte/count limits.

Handler-time updates carry `parent_request_id`; the host derives the resource
owner. Background publishers echo a complete host-issued owner triple accepted
only if previously issued to that process generation. These fields are mutually
exclusive. Omitting both means process-scoped state with no session-owned data.
Revisions increase within a generation; replacements may restart at zero. The
host attaches manifest identity, non-repeating process-instance fence, generation,
and owner, rejects stale/foreign state, and clears state on crash, reload,
disablement, or owner change.

One snapshot is bounded to 128 activities, 256 nodes, 64 actions, 16 tree levels,
256 KiB encoded, 64 KiB detail text, short labels/references, and portable JSON
integer revisions/timestamps. At most 32 snapshots emit per one-second window;
excess valid updates coalesce last-wins into the next window with at most one
bounded diagnostic per throttled window. IDs are extension-scoped. States are
`empty`, `loading`, `pending`, `active`, `running`, `succeeded`, `failed`,
`cancelled`, `degraded`, `stopped`, and `unavailable`.

Optional activity `metrics` contain `tool_calls`, disjoint `input_tokens`,
`cache_read_tokens`, `cache_write_tokens`, `output_tokens`, its
`reasoning_tokens` subset, and optional exact whole-microdollar
`cost_microdollars`. Counters are portable integers and reasoning cannot exceed
output. A frontend may sum the three input buckets for `↑input ↓output`, but
must not add reasoning to output again.

References are `session`, `artifact`, `resource`, or `url`. URLs are sanitized
absolute HTTP(S); credentials, localhost/`.local`, and private/loopback/link-local/
unspecified/multicast literal IPs are rejected. Frontends expose them only after
a user click. Secrets, queries, retrieved content, typed values, and other
untrusted data do not belong in status labels, titles, provenance, or reconnect
state. Actions name commands already declared by that manifest.

Generic extension state stays out of persistent chrome. The coding TUI's one
first-party observed exception is `octet-subagents`: during an owning root run
with workers it renders owner-fenced `subagent` activities as a persistent
transcript event above the composer from native `AgentEvent::DelegationUpdated`
telemetry. The complete bounded roster is never truncated by `Ctrl+O`. The
host-owned footer adds live priced child spend while active, then durable
root-session delegated usage after settlement, never an extension footer string.

`/extensions` opens the installed-bundle management menu. Enter toggles ordinary
bundles or opens the enabled first-party web-search provider picker; activation
does not persist trust grants. Activation is read-only when
project/environment/CLI activation makes user config non-authoritative.
`/extensions status` is the diagnostic and presentation fallback;
`/extensions inspect <agent-session:…>` opens a current parent-bound delegated
transcript; `/extensions action <extension> <action-id>` performs validated
interactive routing.

The enabled package's no-argument `/subagents` opens a live arrow-key worker list.
Owner-bound refresh reconciles authoritative `agent_sessions` state and retains
focus by stable node ID. Enter revalidates and opens the selected bounded read-only
transcript; Escape/Left returns to the list.

Serve carries the same complete state through authenticated snapshot/event
reduction. `extension.invokeAction {extension, extensionInstanceId, generation,
revision, action, confirmed}` binds action identity; reconnect replaces state
without replay. Destructive actions need a second host-owned confirmation and an
instance/generation/revision/action-bound authenticated command. The selected
manifest's process executes its own command even when another extension uses
the same name. At most one matching extension confirmation is preapproved during
that command; other command confirmations fail closed without a trusted surface.
Plain/print/RPC retain bounded text/structured fallbacks and never implicitly
choose an action or selection.

Raw ANSI, HTML, JavaScript, CSS, frontend coordinates, and extension rendering
code are invalid presentation data. Immutable tool results remain evidence.

### Child model-session service

`agent_sessions` is the legacy reverse service for bounded in-harness
orchestration. In the coding product, `octet-subagents` is its sole owner and
observer. Successful negotiation creates an extension-only V2 manager; the host
does not expose parallel native root collaboration tools. Every child enters the
owner-bound tree before running.

Each request includes an active `parent_request_id` from a model-tool call or
declared command. The host derives the durable owner, never accepting a child
parameter as authority. Command ownership lets validated presentation actions
inspect/stop existing children without switching the parent session. An absent
owner/service fails with `-32002`.

[`agent/spawn`](PROTOCOL-REFERENCE.md#213-agentspawn-api-02-working-tree)
retains every field and bound: `{parent_request_id, task_name, profile?,
fingerprint?, message, idempotency_key, policy}`. Mandatory `policy` contains
`tools`, `max_depth`, `max_concurrent_children`, `max_turns`, optional/null
`max_tokens`, `max_cost_microdollars`, `max_output_bytes`, and `timeout_ms`.
The tools are a non-empty subset of `read`, `search`, `edit`, `write`, and `bash`,
the parent's full standard scope by default, narrowable per spawn. Depth is one;
there are at most eight active children and thirty-two retained records. The
host freezes detached tool snapshots, applies lower parent turn/cost ceilings,
inherits resolved model context/output limits, caps UTF-8 returned output, and
owns the absolute wall timer. There is no separate fixed aggregate extension
reservation pool. Children receive no collaboration tools and cannot run a
follow-up themselves. Null `max_tokens` exactly inherits the parent's optional
cumulative session ceiling, including none; a non-null value is an optional
stricter cap.

[`agent/list`](PROTOCOL-REFERENCE.md#216-agentlist-api-02-working-tree)
retains the complete record: public task/profile, idempotency key/fingerprint,
effective policy, created/started/completed/deadline timestamps, turn/token/cost
usage, terminal `timed_out`, and owner/principal provenance, including current
structured phase/tool, host-observed call count, and disjoint token buckets.
`session` is an opaque `agent-session:*` reference, never the private JSONL path.
Serve resolves it only through a host-written parent-session/extension-principal/
resource-owner binding into a locked read-only inspector. `/extensions inspect`
opens only the current parent's delegation team.

Every child has a fresh independent context; child tokens never enter parent
request context. On root settlement, the host stops and briefly joins children,
aggregates durable usage/cost including picodollar remainders, and writes one
`delegated_agent` usage record per child to the root before its checkpoint. This
is accounting, not context sharing: delegated spend is cumulative, durable, and
not double-counted in the footer or later cost checks. Read-only list/wait remain
owner-scoped after the root becomes inactive so telemetry and inspection can
settle; spawn/message/follow-up/interrupt require an active owner and fail closed.

Idempotency is scoped to extension principal plus resource owner. Identical
task/profile/fingerprint/input/policy retries return the same retained child;
different input with the same key fails. At the next owning run, stale entries
whose records were cleared are pruned. The orchestrator retains bounded terminal
summaries/errors and sibling roster; an identical explicit retry replaces a
matching orphaned cache entry before requesting a new child. The stable principal
is manifest name plus SHA-256 manifest-identity digest, never manifest path.

Malformed parameters with parseable IDs return `-32602`; service, ownership,
limit, persistence, and operation failures return `-32002`. Every target must
belong to the principal/owner's trees. Follow-up resumes a settled child durably;
shut-down/orphaned/still-stopping targets are rejected. `agent/wait` defaults to
30 seconds and caps at 60 seconds. Parent settlement cancels outstanding reverse
requests; extension shutdown stops owned child trees. Trees use principal plus
durable owner, not generation, so supervised restart/reload can resume them;
a full process-host rebuild creates a new service boundary.

The exact steering/follow-up/list/wait/interrupt requests and responses remain in
[legacy reverse methods](PROTOCOL-REFERENCE.md#214-agentmessage-api-02-working-tree).
The host owns actual conversations, persistence, permission inheritance, and
resource limits; the extension owns orchestration. Hosted-agent services are
separate. Observe children with list/wait: child turns do not fan out into
extension `session/*` or `turn/*` notifications, which cover the owning/root
product session.

### Optional kernel services

Generic brokers remain distinct from domain protocols:

- [`artifact/publish`](PROTOCOL-REFERENCE.md#28-artifactpublish-api-02) returns
  verified media IDs bound to the active host-derived owner and generation.
  Without that owner it fails `-32002`; foreign owner/generation resolution
  sees an unavailable artifact. Ingestion checks size, SHA-256, supported MIME
  signature, containment, owner, and generation.
- [`policy/evaluate`](PROTOCOL-REFERENCE.md#29-policyevaluate-api-02) carries
  host-classified intent. Conditional negotiated `approvals` lets an approved
  `ask` return a short-lived token. Retry the same intent under the same active
  owner request with that token; redemption consumes it atomically. Expiry,
  reuse, or changed intent/generation/parent denies and invalidates a presented
  live token. Tokens appear only with `ask`, never `allow`/`deny`.
  `policy_intents` alone does not imply approvals.
- [`secret/get`](PROTOCOL-REFERENCE.md#210-secretget-api-02) requires a
  conditionally offered/negotiated broker and the exact manifest allowlist.
  The host supplies principal, complete owner triple, active parent, and name
  to the broker. No owner/service gives `-32002`, undeclared names `-32602`,
  and missing/broker-failed values the same `-32004` `secret is unavailable`.

The coding product currently leaves approvals off, configures no secret broker,
and answers generic policy intents with `deny`. It offers neither `approvals`
nor `secrets`, despite implemented legacy host services and Python helpers.

API `0.2` confirmation, input, artifact, policy, secret, and child-session
requests require an active `parent_request_id`. Parent settlement cancels every
unresolved child. Progress is strictly monotonic per parent; inactive/stale
events are ignored and progress remains ephemeral.

Model-tool/tool-hook contexts and the coding product's slash-command,
`before_prompt`, `after_response`, and `context/collect` boundaries carry:

```json
{
  "session_id": "durable-session-owner",
  "extension_instance_id": "host-created-instance-fence",
  "process_generation": 3
}
```

This host-derived `resource_owner` must namespace browser tabs, MCP/LSP
connections/documents, memory handles, and comparable state, never a model-supplied
identifier. `session_id` is SHA-256-derived and stable when reopening the same
persisted session at its canonical path. Instance IDs change on a full process-host
rebuild even if generation numbering restarts; generation fences reload/restart
within an instance. API `0.1` omits this field. Current coding-product ambient
status/renderer calls and ownerless unsolicited contexts remain process-scoped
and must not allocate session-owned handles. The native adapter can preserve a
supplied status/renderer owner and refresh its instance/generation fences.
An owner in prompt/context does not authorize reverse services: active-parent and
service gates still apply. `after_response` is success-only synchronization;
settled events own failure/cancellation/cleanup observations.

[`input/request`](PROTOCOL-REFERENCE.md#27-inputrequest-api-02) sends
`{parent_request_id, prompt, secret}` and returns `{value: string|null}`. Prompts
are non-whitespace, at most 16 KiB UTF-8; answers at most 256 KiB. Null is
cancellation. Secret replies use a private channel and never enter diagnostics,
progress, sessions, or persistence. Headless/unavailable input is cancelled.

Policy decisions are `allow`, `ask`, or `deny` with optional `approval_token`;
hints cannot lower host policy. Tokens are 64-character lowercase hexadecimal
single-use retry capabilities bound to original canonical intent, generation,
active parent/owner, and bounded expiry, not durable permissions.
`confirmation/request` is cooperative UI, not enforcement.

`secret/get` sends `{parent_request_id, name}` and returns `{value: string}`.
Names follow the duplicate-free manifest identifier rules above; UTF-8 values
are capped at 64 KiB. The host neither logs nor persists them and best-effort
wipes broker and serialized writer buffers. The extension receives an ordinary
process-memory string: there is no end-to-end zeroization. Keep secrets
short-lived and out of results, progress, diagnostics, and storage.

Frontend contributions are plain text plus optional semantic roles, never raw
terminal escapes. Tool-renderer segments are retained as internal provenance,
not rendered in the TUI or exposed by Ctrl+O, `/verbose`, transcript selection,
or copy. Original tool results remain immutable protocol/persistence/export
redaction evidence, not a presentation surface. Header/status/footer,
notification, and confirmation remain separate protocol surfaces. The coding TUI
does not request/render generic persistent header/status/footer; its one observed
exception is the host-owned `octet-subagents` metrics renderer, never extension
rows or footer values.

Interactive tool/command confirmations open typed allow/deny panels. Dropped,
non-interactive, or out-of-active-boundary requests are denied, never implicitly
accepted.

[API `0.2` cancellation](PROTOCOL-REFERENCE.md#19-cancelrequest-api-02) is
cooperative and request-scoped. Before writing begins it skips a queued frame;
afterwards the frame finishes and at most one `$/cancelRequest` follows. The
SDK exposes an ambient token and returns `-32800` if cancellation wins. The host
drops the waiter, tombstones late replies without harming unrelated calls,
cancels correlated child requests, and terminates non-cooperative generations
after bounded grace. Cancellation promises neither rollback nor unsafe replay.

`/extensions` manages installed bundles: Up/Down moves, Enter toggles, Escape
closes. Selecting enabled `octet-web-search` opens a provider picker; Brave
Search is recommended and requests its key through correlated secret input;
SearXNG remains available. Only `enabled_extensions` changes, never trust
grants; provider state remains extension-owned. Activation is read-only if
project, environment, or CLI layers participate, because user config is not
next-launch authority; already running web-search setup remains available.
Precedence is revalidated immediately before each write. Enabled unavailable
bundles remain disable-only. Source-changing trust, tool-name collisions, and
explicit required-tool removal fail closed.

`/extensions status` includes the selected manifest path and a copyable exact
persistent/one-shot trust grant for enabled-but-untrusted entries. These grants
do not bypass safe-mode startup denial.
`/extensions reload` replaces running processes after successful handshakes;
general `/reload` reruns discovery and rebuilds the product boundary.

## Python SDK

The dependency-free `octet-extension-sdk` exposes `octet_extension.Extension`
for **legacy API `0.1`/`0.2` only**. From a checkout:

```console
python3 -m pip install ./sdk/python
```

Decorated tools/commands plus `Extension.run()` provide JSON-RPC framing, one
serialized stdout writer, bounded concurrent dispatch, negotiation, ambient
cancellation, scoped progress, ephemeral input, correlated host requests, and
bounded shutdown/drain. Diagnostics use structured stderr; shutdown/stdin close
exit the process. API `0.1` retains its wire.

```python
from octet_extension import Extension

ext = Extension(api_version="0.2")

@ext.tool(
    name="hello_world",
    description="Greet someone",
    output_schema={"type": "object", "properties": {"greeting": {"type": "string"}}, "required": ["greeting"]},
)
def hello(args):
    return {
        "content": [{"type": "text", "text": "Hello!"}],
        "structured_content": {"greeting": "Hello!"},
    }

ext.run()
```

This retained legacy illustration is not an API `0.3` quickstart. Bootstrap tool
and command names match the manifest; later negotiated dynamic tools need not be
manifest entries. Hook/context/status/renderer decorators and `notify()`/
correlated `confirm()` remain documented in the
[complete legacy Python runtime reference](../../sdk/python/legacy-runtime.md).
Generated API `0.3` types do not implement a complete `Extension` runtime.

## Lifecycle and reload

`ExtensionProcess` implements native `Extension`; negotiated tools register via
`ExtensionHost` with normal duplicate detection and non-replayable safety defaults.
Frontends call typed command/hook/context/status/renderer/notification/confirmation
and API `0.2` lifecycle APIs at semantic boundaries.

Legacy lifecycle subscriptions are exact `session/started`, `session/settled`,
`turn/started`, `turn/settled`, `tool/started`, and `tool/settled` names. Settled
outcomes cover completion, failure, cancellation, interruption, frontend
disconnection, shutdown, and limits across interactive/plain/print/RPC/native-host/
Serve boundaries. Notifications are best effort; host cleanup/persistence remain
authoritative. API `0.1`/`0.2` `after_response` remains success-only; in `0.2`
it is bounded response-content synchronization, not terminal cleanup.

### Declared API `0.3` session hooks

These are not legacy lifecycle notifications. The complete paired declaration,
selection, sanitized binding/end payload, 250 ms dispatch, idempotence,
non-veto, and reload/crash fencing rules moved to
[declared API `0.3` session hooks](../extensions.md#declared-api-03-session-hooks).

For admitted running full-access extensions, reload starts and fully initializes
a candidate while the old process remains ready. Launch, handshake, or
contribution mismatch leaves the old process active. Negotiated dynamic catalogs
may change; static tools and all command/hook/UI contributions must remain
compatible or return `re-registration required` for an intentional frontend
rebuild. On acceptance the old generation stops admission, drains to a bounded
deadline, cancels the remainder, emits lifecycle terminals, and reaches shutdown
acknowledgement or timeout. The host seeds replacement lifecycle state and
atomically cuts new calls over. Confirmations, progress, artifacts, approvals,
secret lookups, and other child operations never cross generations.

Shutdown is graceful but bounded. Non-exiting processes are killed; dropping the
last runtime handle uses kill-on-drop cleanup. Status exposes generation, API,
negotiated features, pending count, health, and bounded last error. See the
[exact shutdown stages](PROTOCOL-REFERENCE.md#18-shutdown).

The supervisor watches successfully initialized residents. Unexpected exit or
terminal transport failure removes the dead tool group and enters `backoff`.
Candidate-first restart uses full-jitter exponential delay, 250 ms base and 30 s
cap. Eight failed attempts or a permanent manifest/version/re-registration error
parks it; 30 seconds continuously ready resets the budget. Shutdown cancels
supervision. Manual and supervised reload share a generation-checked lock;
interrupted unsafe work is never replayed. Initial launch/handshake failures are
parked discovery entries: supervision begins only after one successful
initialization. There is no live-process heartbeat. A full product rebuild
creates a new instance/supervisor, resetting in-memory parked/backoff history.

Sleep inhibition is extension-owned; no core inhibitor exists. The `caffeinate`
example is API `0.2`, version `0.2.0`, reference-counting owning/root turn
start/settle observations and clearing on session settlement. One bounded macOS
`/usr/bin/caffeinate` helper is fenced by shutdown and host process-group cleanup.

Retained legacy examples:

- [hello-world](../../examples/extensions/hello-world/README.md): minimum process
  and legacy handshake plus contribution points.
- [caffeinate](../../examples/extensions/caffeinate/README.md): API `0.2`
  terminal lifecycle ownership, overlapping-turn counting, bounded macOS sleep
  inhibition, status contribution, and explicit shutdown.
- [git-tools](../../examples/extensions/git-tools/README.md): bounded custom tool,
  command, and semantic renderer.
- [local-model-workflow](../../examples/extensions/local-model-workflow/README.md):
  prompt hooks, deterministic context, status, and notifications.

## Capability boundaries

- **Web search** retrieves/ranks results, not interactive tabs or browser control.
- **Browser use** owns page/tab state and semantic interaction under an extension
  resource owner, not general OS control.
- **Computer use** drives desktop/application UI and needs its own approval and
  observation boundary.
- **Hosted agents** are remote provider services, not octet child conversations.
- **In-harness subagents** are octet model sessions created through the bounded
  host service and orchestrated by an extension.

MCP, LSP, memory, and caffeinate remain separate domains too. A common transport
does not merge permissions, resource ownership, failure policy, or tool semantics.

## Installable extension bundles

Catalog commands select the package matching the running host version. For
octet 0.7.5 availability, signed assets, and public-install verification, consult
the [version-pinned GitHub release](https://github.com/skaft-software/octet/releases/tag/v0.7.5).
Use a reviewed source or local archive when matching publication has not been
verified.

Executable bundles use runtime `extension.toml`, not Serve's application launcher
manifest. An archive has exactly one root named for the extension, all regular
runtime files, and optional docs, fixtures, and skills:

```text
octet-web-search/
├── extension.toml
├── extension.py
├── install.json             # written by octet, not shipped in the archive
└── skills/
    └── octet-web-search/
        └── SKILL.md
```

Select a bundle-supported API and exact octet compatibility independently of the
extension version. New packages follow [API `0.3`](API-0.3-REFERENCE.md).
Existing API `0.2` packages remain installable; API `0.1` is supported only
unpackaged for legacy runtime compatibility. This is retained **legacy package
metadata**, not a version to substitute into a new authoring guide:

```toml
name = "octet-web-search"
version = "0.7.5"
api_version = "0.2"
requires_octet = "=0.7.5"
```

`requires_octet` is optional for unpackaged local copies but enforced when
present. Installed bundles require an exact match to the running octet version.
The first-party catalog is `octet-browse`, `octet-mcp`, `octet-subagents`, and
`octet-web-search`. The four bundled manifests still declare API `0.2`;
do not retag them to claim API `0.3` support.

After matching publication is verified:

```console
octet extension install octet-web-search
octet extension list
octet extension update octet-web-search
octet extension remove octet-web-search
```

Third-party/offline bundles use local archives, not arbitrary URLs or registries:

```console
octet extension install --path ./my-extension-0.1.0.tar.gz
octet extension update --path ./my-extension-0.2.0.tar.gz
```

Local update archives must have the same managed package ID and use the same
validation, swap, and rollback rules. Official installs download the archive and
release `SHA256SUMS` over HTTPS, then verify the digest. Local installs compute
and record SHA-256. The host rejects oversized archives, entries, manifests,
paths, file counts, expanded data; non-UTF-8/non-portable paths; multiple roots;
duplicates; links/devices/special entries; directory/manifest mismatch; API/exact
octet incompatibility; and unsafe relative entrypoints.

Regular files extract into a same-filesystem private staging directory and
publish to `~/.octet/extensions/<id>/` by atomic rename. Update validates the
complete candidate before moving the current package, rolling back that move on
publication failure so the prior bundle remains usable.

`install.json` records schema, ID, extension/API versions, exact octet requirement,
official URL or canonical local source, archive digest, and installing octet
version. Removal accepts only managed bundles and deletes only their directory.
Config, provider state, sessions, artifacts, browser profiles, and other data
must live outside it and are not removed.

Installation/discovery never enables or starts an extension, records a trust
grant, or grants capabilities. `/extensions` may persist activation; it never
records a trust grant. Coding-product full access implicitly trusts the selected
extension without persisting trust. `--enable-extension` is invocation-only
activation; `--trust-extension` is an optional invocation-only explicit grant
that does not enable anything. `--safe-mode` does not inherit implicit trust and
keeps executable extensions stopped even with explicit grants; the `unsafe_host`
floor and independent process gates still apply.

Packaged `skills/*/SKILL.md` become user-installed skill candidates but stay
inactive until explicitly loaded. `~/.octet/skills` and explicit `--skill-dir`
have higher precedence.

There is no install hook: installation never invokes `pip`, downloads a browser,
provisions a model, starts a server, or runs extension code. Runtime dependencies
and explicit setup are the extension's responsibility and must be documented.
Local packages have no remembered remote update source; only published-catalog
updates download. There is no automatic earlier-first-party hotfix migration,
old-root scan, or retired-package cleanup. Existing Ygg installations/external
data remain separate and untouched.

## First-party application packages

The complete Serve application package is separate from executable bundles. It
uses `package.toml`, contains target-specific `bin/octet-serve-runtime`, and is
never loaded by executable-extension discovery. These catalog commands also
remain gated on matching publication:

```console
octet extension install octet-serve
octet extension update octet-serve
octet extension remove octet-serve
octet serve
```

Installation under `~/.octet/extensions/octet-serve/` has its own manifest,
executable, and `install.json`. The application manifest declares ID/version,
exact octet version, target triple, launcher arguments, executable SHA-256, and
loopback/process/workspace capabilities. Official installation uses a matching
target archive and shared release `SHA256SUMS`; local archives use:

```console
octet extension install --path ./octet-serve-0.7.5-TARGET.tar.gz
```

The application archive retains its strict two-file payload and atomic install.
`octet serve` revalidates compatibility/checksum before replacing the launcher
process. As a first-party replacement octet process it inherits launcher config
and provider environment, not the sanitized child environment used for
model-controlled tools and executable extensions. Removal deletes only package
files; sessions, project metadata, and other user data remain outside the directory.
