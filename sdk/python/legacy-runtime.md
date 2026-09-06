# Legacy Python extension runtime

This is the retained **API `0.1`/`0.2` maintenance reference** for
`octet_extension.Extension`, not a current-API quickstart. New authoring uses
[API `0.3`](../../docs/extensions.md); generated `octet_extension.api_v03`
models and canonical validators do not implement a complete API `0.3`
`Extension` runtime. Do not retag a legacy manifest or silently translate wires.

**Identity boundary:** octet 0.7.0 source uses `octet_version`, `requires_octet`,
`OCTET_*`, and `octet_extension`, not aliases for old Ygg names. First-party source
SDK/extension distributions are version `0.7.0`; independent examples retain
their own versions. Native octet publication does not publish the SDK to PyPI.

`octet-extension-sdk` is dependency-free. It owns JSON-RPC 2.0 JSON-lines
framing, flushes each response, validates initialization against the selected
manifest, and uses structured stderr logs for diagnostics. octet owns model
conversations, transport/supervision, persistence, permissions/approvals, cleanup,
and shared limits. The process owns its capability: MCP, search, browser,
computer use, memory, LSP, subagents, caffeinate, or another replaceable domain.
Artifacts and child sessions are generic services, not host domain managers.

API `0.1` remains the runtime default for existing extensions. API `0.2` adds
negotiated concurrency, cooperative cancellation, correlated progress/host
requests, ephemeral input, structured/media results, artifacts, lifecycle events,
live tool catalogs, child sessions, graceful drain, and conditional single-use
approvals/secrets. A legacy `api_version = "0.2"` manifest makes the host export
matching `OCTET_EXTENSION_API_VERSION`, followed by the SDK automatically.
Explicitly override the constructor only for a standalone harness/test:

```python
ext = Extension(api_version="0.2", max_concurrent_requests=4)
```

Install the source package from a checkout:

```console
python3 -m pip install ./sdk/python
```

This retained minimal example uses the **legacy default wire**:

```python
from octet_extension import Extension

ext = Extension()

@ext.tool(name="hello_world", description="Greet someone")
def hello(args):
    name = args.get("name", "world")
    return {"content": f"Hello, {name}!"}

ext.run()
```

Bootstrap tools must appear in the manifest:

```toml
[contributes]
tools = ["hello_world"]
```

The host sends manifest, workspace, and session/model state in `initialize`.
Bootstrap tool/command decorators must exactly match declarations or the handshake
rejects them. Later negotiated dynamic tools need not be listed. Initialize is
epoch `0`, the only deterministic first-request catalog: put turn-one tools
there; the host does not wait for registration quiescence.

## Contribution points

```python
@ext.command(name="checkpoint", description="Preview a checkpoint")
def checkpoint(arguments, context):
    return {"text": "..."}

@ext.hook("before_prompt")
def before_prompt(payload, context):
    return {"disposition": {"action": "continue"}}

@ext.context
def context(params):
    return [{"label": "example", "content": "...", "placement": "system_suffix"}]

@ext.status("status")
def status(params):
    return {"surface": "status", "text": "ready", "priority": 0}

@ext.renderer("hello_world")
def render(params):
    return {"segments": [{"text": "hello", "style_role": None}]}

# Call after initialize, with `[contributes] presentation = true` (API 0.2).
def publish_ready_after_initialize():
    ext.publish_presentation({
        "revision": 0,
        "status": {"state": "active", "label": "ready"},
        "activities": [],
        "actions": [],
    })
```

Two-parameter tools receive `(arguments, context)`; one-parameter tools receive
`arguments`. Commands receive argument arrays, hooks their payload, and
context/status/renderer handlers their complete parameter object. Each may accept
a second ambient context argument.

Legacy hook payloads are:

- `before_prompt`: `{"prompt": string}`
- `after_response`: `{"response": string}`
- `before_tool_call`: `{"name": string, "arguments": object}`
- `after_tool_call`: `{"name": string, "arguments": object, "output": string,
  "is_error": bool}`

`after_response` is success-only in both APIs, not called after failure,
cancellation, interruption, disconnection, or shutdown. API `0.2` uses lifecycle
observations for cleanup and this hook only for bounded response synchronization.
Tool handlers may return `content`, `is_error`, and `metadata`, but the API `0.1`
native adapter discards metadata: it has no frontend/renderer/persistence guarantee.
Generic status/header/footer and renderer contributions do not become coding-TUI
chrome; see the [host presentation boundary](../../docs/extensions/legacy-authoring.md#optional-kernel-services).

## API 0.2 negotiation and scheduling

A legacy host with every optional service enabled may send:

```json
{
  "version": "0.2",
  "required_features": ["request_cancellation", "content_parts"],
  "optional_features": [
    "request_progress", "artifacts", "lifecycle_events", "policy_intents",
    "dynamic_tools", "agent_sessions", "approvals", "secrets"
  ],
  "limits": {"max_concurrent_requests": 4}
}
```

The SDK rejects unsupported required features, returns the supported subset,
caps concurrency locally, and ignores unknown optional features. Supported
features are `request_cancellation`, `content_parts`, `request_progress`,
`artifacts`, `lifecycle_events`, `policy_intents`, `dynamic_tools`,
`agent_sessions`, `delegation_telemetry_v1`, `approvals`, and `secrets`.

The host requires `delegation_telemetry_v1` for first-party `octet-subagents`
when offering `agent_sessions`; incompatible bundles fail with a reinstall
diagnostic. The child service is offered only when safely owner-bound; the coding
product enables it only for trusted/enabled `octet-subagents`. Approvals require
configured issuance plus `policy_intents`; secrets require a broker plus a
non-empty manifest allowlist. The coding product leaves approvals off, has no
secret broker, and denies generic policy intents, so neither feature is offered.

The stdio reader only decodes and queues; it never invokes extension code.
Handlers use a bounded worker pool and separate admission queue
(`max_pending_requests`, default 64). One dedicated writer owns stdout and
serializes complete frames. API `0.1` uses that transport with one handler worker
for sequential behavior. After initialization:

```python
ext.negotiated_features       # frozenset[str]
ext.negotiated_concurrency    # int
```

The [legacy wire reference](../../docs/extensions/PROTOCOL-REFERENCE.md#11-initialize)
retains exact initialization shapes, validation, and host feature availability.

## Semantic presentation snapshots

API `0.2` manifests may declare `[contributes] presentation = true` and call
`publish_presentation()` (alias `presentation`) only after initialization.
Handler calls automatically carry `parent_request_id` for host-derived ownership.
Background publishers pass the complete host-issued `resource_owner=` triple;
omitting both means process-scoped state with no session-owned data. Revisions
increase within a process; runtime reset/reload begins a new sequence.

A snapshot has optional compact `status`, bounded `activities`, optional
`list`/`tree` `collection` with stable nodes/selected detail, and `actions`
routed only to commands declared by that manifest. States are `empty`, `loading`,
`pending`, `active`, `running`, `succeeded`, `failed`, `cancelled`, `degraded`,
`stopped`, and `unavailable`. References are `session`, `artifact`, `resource`,
or a host-vetted user-clicked HTTP(S) `url`.

The SDK checks 256 KiB snapshots, 128 activities, 256 nodes, 64 actions, portable
and monotonic revisions. The host owns complete schema, tree parentage/depth,
IDs, URL safety, command routing, 32-updates/second generation rate, owner/generation
fencing, and frontend reduction. Snapshots are not model results; no rendering
code, secrets, queries, retrieved content, typed values, credentials, or private
data belongs in labels/reconnect state. Tool results remain immutable evidence.
See [exact presentation rules](../../docs/extensions/PROTOCOL-REFERENCE.md#25-presentationupdate-api-02).

## Dynamic tool catalogs

After negotiating `dynamic_tools`, an initialized extension can add/replace/remove
tools without restart:

```python
def search_handler(args):
    return f"result for {args['query']}"

update = ext.register_tool(
    name="provider_search",
    description="Search the selected provider",
    parameters={
        "type": "object",
        "properties": {"query": {"type": "string"}},
        "required": ["query"],
    },
    handler=search_handler,
)
# {"revision": 1, "tools": ["provider_search", ...]}

ext.unregister_tool("provider_search")
```

`register_tools([...])` sends several complete definitions in one request,
replacing schema and handler for existing names. `unregister_tools(*names)`
sends one removal request; absent names are harmless. Definitions contain `name`,
`description`, callable `handler`, and optional `parameters`/`output_schema`.
Each request and the complete catalog are capped at 256 tools. Accepted mutations
appear at the next model-request boundary, not necessarily the first request
after initialization.

Mutations are transactional: stage a local catalog, wait for acknowledgement,
then commit only host-accepted names. Rejection leaves local state unchanged.
The response must be the next monotonic epoch or the SDK raises a protocol error.
`ext.tool_catalog_revision` reports the last committed epoch, starting at `0`
on initialize/restart. Reload also begins from initialize epoch `0`.

A model request freezes schema/implementation and receives calls pinned by
`tool/call.catalog_revision`. Python retains eight committed catalogs, plus
makes the staged next catalog addressable during publication-before-ack. Unknown
or retired epochs fail `-32602`, never silently dispatch to newer handlers.
See [transaction and failure rules](../../docs/extensions/PROTOCOL-REFERENCE.md#211-toolsregister-api-02).

## Child model sessions

With offered/negotiated `agent_sessions`, a legacy tool handler can orchestrate
bounded in-harness children. This is a host-service illustration, not a claim
that arbitrary extensions receive the coding product's first-party-only service:

```python
from octet_extension import text_content, tool_result

@ext.tool(name="orchestrate", description="Delegate a bounded investigation")
def orchestrate(args):
    child = ext.spawn_agent(
        task_name="inspect-catalog",
        profile="review",
        fingerprint="0123456789abcdef" * 4,
        message="Inspect the current provider tool catalog.",
        idempotency_key=f"catalog:{ext.request_id}",
        tools=["read", "search"],
        max_depth=1,
        max_concurrent_children=2,
        max_turns=8,
        max_tokens=None,
        max_cost_microdollars=200_000,
        max_output_bytes=8_192,
        timeout_ms=300_000,
    )
    ext.send_agent_message(child["agent_id"], "Include resource tools.")
    ext.follow_up_agent(child["agent_id"], "Return a compact summary.")
    settled = ext.wait_agents(timeout_ms=30_000)
    agents = ext.list_agents()
    if settled["timed_out"]:
        ext.interrupt_agent(child["agent_id"])
    return tool_result(
        text_content(f"Observed {len(agents['agents'])} owned child sessions.")
    )
```

Exact helpers:

- `spawn_agent(*, task_name, message, idempotency_key, tools, max_depth,
  max_concurrent_children, max_turns, max_tokens=None, max_cost_microdollars,
  max_output_bytes, timeout_ms, profile=None, fingerprint=None,
  parent_request_id=...)`;
- `send_agent_message(target, message, *, parent_request_id=...)`;
- `follow_up_agent(target, message, *, parent_request_id=...)`;
- `list_agents(*, parent_request_id=...)`;
- `wait_agents(*, timeout_ms=30_000, parent_request_id=...)`;
- `interrupt_agent(target, *, parent_request_id=...)`.

`max_tokens=None` exactly inherits the parent's optional cumulative session-token
ceiling, including none. Non-null 1,000..=64,000 requests a stricter cap. It does
not share prompt context: children have fresh contexts and inherit model context
window/resolved per-request output limits. Mirrored root usage is accounting,
not parent prompt tokens or consumption of the parent's own-context ceiling.

Model-tool/host-owned-command handlers supply ambient parent IDs. Outside them,
pass an explicit active model-tool/command parent. Command `resource_owner`
bindings allow declared actions to inspect/stop owned children. Prompt/context
handlers may have owners for namespacing but are not active parents and must not
spawn children. Helpers require negotiation and validate basic response shapes.
`wait_agents` accepts 1..=60,000 ms. Task/profile names are 1..=48 lowercase ASCII
letters, digits, underscores, or hyphens; optional fingerprints are lowercase
SHA-256 digests. The host retains these with keys, policy, and timestamps for
restart recovery through list. Task/steering/follow-up text is capped at 128 KiB;
the SDK also rejects empty messages.

Spawn retry safety depends on required principal/owner-scoped idempotency keys.
Identical retries return the same child; different input with the same key fails.
Malformed wire parameters with a parseable ID yield `-32602`; unavailable
owner/service and rejected operations yield `-32002`. Targets must belong to the
principal/owner's trees. The host owns conversations, persistence, inherited
permissions, and limits; the extension owns orchestration. Trees use principal
plus durable owner, not generation, so restart/reload can resume them. Explicit
extension shutdown stops owned trees.

These are local in-harness children, not hosted jobs. List/wait observe child
state; child turns do not arrive through extension `session/*`/`turn/*`
notifications, which observe owning/root sessions. The corresponding
implementation checks are Python helper/negotiation tests and Rust cross-process
host-service gates; run them against the implementation being qualified.
Full policy fields,
bounds, records, and failure rules remain in
[the legacy service reference](../../docs/extensions/PROTOCOL-REFERENCE.md#213-agentspawn-api-02-working-tree).

## Cancellation and progress

Every API `0.2` handler has an ambient thread-safe cancellation token. Poll
between effects, wait during interruptible work, or raise its standard error:

```python
from octet_extension import current_cancellation

@ext.tool(name="fetch_many", description="Fetch several records")
def fetch_many(args):
    token = current_cancellation()
    for index, item in enumerate(args["items"]):
        token.raise_if_cancelled()
        fetch(item)
        ext.progress(
            message=f"Fetched {index + 1} of {len(args['items'])}",
            current=index + 1,
            total=len(args["items"]),
            unit="records",
        )
    return "done"
```

`ext.cancellation` and `ext.request_id` expose the same ambient values.
Cooperative cancellation settles the original ID with `-32800`; it is idempotent
and a normal result already winning the terminal race remains the sole result.
It does not promise rollback or authorize replay of ambiguous unsafe work.

`ext.progress(...)` emits `$/progress` with sequences beginning at 1 and
increasing per request. It accepts status convenience arguments or an event:

```python
ext.progress({
    "type": "output",
    "stream": "stderr",
    "encoding": "utf8",
    "data": "retrying request",
})
```

Progress needs negotiated `request_progress` and an active parent. It is ephemeral
protocol traffic, not tool-result content. See
[cancellation](../../docs/extensions/PROTOCOL-REFERENCE.md#19-cancelrequest-api-02)
and [progress bounds](../../docs/extensions/PROTOCOL-REFERENCE.md#26-progress-api-02).

## Structured results and artifacts

API `0.2` returns typed content parts. String returns convert to one text part;
helpers make the full result explicit:

```python
from octet_extension import image_content, text_content, tool_result

@ext.tool(
    name="capture",
    description="Capture a screenshot",
    output_schema={
        "type": "object",
        "properties": {"url": {"type": "string"}},
        "required": ["url"],
    },
)
def capture(args):
    artifact_id = ext.publish_artifact(mime_type="image/png", data=image_bytes)
    return tool_result(
        text_content("Captured the page."),
        image_content(artifact_id, "image/png", alt="Page after submit"),
        structured_content={"url": args["url"]},
        metadata={"cache": "miss"},
    )
```

`audio_content(..., transcript=...)` uses the same artifact boundary. Image
`alt` and audio `transcript` are optional. Only text and host artifact references
are content parts: arbitrary local paths and remote URLs are not media results.
Structured content/metadata are retained independently of compact model text.

`publish_artifact` accepts bounded inline bytes or a relative scratch path with
explicit size and SHA-256:

```python
artifact_id = ext.publish_artifact(
    mime_type="image/png",
    path="captures/result.png",
    size=byte_count,
    sha256=digest,
)
```

Python rejects absolute/parent-traversing paths. The host safely opens files,
verifies type/size/digest, ingests bytes, and returns an opaque ID requiring the
active host-derived owner. Only the same owner/generation can resolve it; leaked
foreign IDs are unavailable. `output_schema=` accompanies `parameters=` and
validates required `structured_content`; API `0.1` forbids output schemas.

All limits and supported MIME signatures are retained in
[artifact publication](../../docs/extensions/PROTOCOL-REFERENCE.md#28-artifactpublish-api-02)
and [tool results](../../docs/extensions/PROTOCOL-REFERENCE.md#12-toolcall):
256 KiB inline, 20 MiB/artifact, 64 MiB and 64 artifacts/generation; 256 content
parts with explicit text, 64 MiB referenced media/result, 256 KiB structured
content, 64 KiB metadata, and their depth/node/key bounds. The full 1 MiB legacy
frame limit still applies. These are **legacy media facilities**, not foundation
API `0.3` image/audio support.

## Parent correlation and lifecycle

API `0.2` handler `request(...)` calls carry ambient `parent_request_id`.
`confirm(...)`, `request_input(...)`, `publish_artifact(...)`,
`evaluate_policy(...)`, `get_secret(...)`, and child-session helpers require it;
outside a handler pass an explicit active `parent_request_id=`. API `0.1` frames
remain unchanged.

Model-tool/tool-hook and coding-product command/`before_prompt`/`after_response`/
`context/collect` contexts carry `context["resource_owner"]`: durable
host-derived `session_id`, instance ID, and generation. Namespace browser tabs,
MCP/LSP connections, memory handles, and other state by the full triple; never
accept a model-supplied owner. Instance changes across full host rebuilds even if
generations restart; generation changes on reload/automatic restart. Old handles
are invalid when either fence changes. Current coding-product ambient
status/renderer calls and ownerless unsolicited contexts remain process-scoped
and must not allocate session state. The native adapter can preserve supplied
status/renderer owners and refresh their instance/generation fences. Ownership alone does
not grant reverse services; active-parent/feature gates still apply.

Typed ephemeral input inside a handler:

```python
password = ext.request_input("Password:", secret=True)
if password is None:
    return tool_result(text_content("Input was cancelled."), is_error=True)
```

Wire `input/request` sends `{parent_request_id, prompt, secret}`, returning
`{value: string|null}`. Prompts are non-whitespace and at most 16 KiB UTF-8;
values at most 256 KiB, also subject to the frame bound. `None` means cancellation
or unavailable/headless input. Secret replies use a private channel; Python does
not log them or put them in progress, metadata, or results. Immutable Python
strings cannot be reliably wiped: keep secrets short-lived, never copy, log,
persist, or return them.

Lifecycle subscriptions accept slash or underscore names:

```python
@ext.on_lifecycle("turn_settled")
def turn_settled(event):
    ext.log.info(
        "turn settled",
        turn_id=event.get("turn_id"),
        outcome=event.get("outcome"),
    )
```

Methods are `session/started`, `session/settled`, `turn/started`, `turn/settled`,
`tool/started`, and `tool/settled`. Registered subscriptions return through
`protocol.lifecycle_events`. With no handlers, Python does not negotiate the
feature; an empty negotiated subscription means all at the host. Observations
cannot veto transitions.

The legacy [caffeinate example](../../examples/extensions/caffeinate/README.md)
is API `0.2`, version `0.2.0`. It counts owning/root turn starts/settlements,
clears session state on settlement, and stops its bounded macOS helper on
shutdown. Sleep inhibition is extension-owned, not a core facility.

Submit structured policy before host-managed effects. With optional approvals,
an approved `ask` supplies a one-use token for an exact intent retry:

```python
intent = {
    "kind": "external_side_effect",
    "operation": "browser.submit_form",
    "target": {"origin": "https://example.com", "label": "Publish comment"},
    "data_classes": ["user_text"],
    "adapter_hints": {"read_only": False, "destructive": False},
}
policy = ext.evaluate_policy(intent)
if policy["decision"] == "ask" and policy.get("approval_token"):
    policy = ext.evaluate_policy(intent, approval_token=policy["approval_token"])
if policy["decision"] != "allow":
    return tool_result(text_content("The host denied the action."), is_error=True)
```

`evaluate_policy` requires `policy_intents` and ambient parent. Hints are not
authority; only the host returns `allow`/`ask`/`deny`. `approval_token=` requires
negotiated `approvals` and exactly 64 lowercase hexadecimal characters. Python
accepts returned tokens only with `ask`. The host binds original canonical intent,
active owner/parent, generation, and short expiry, consuming on retry; mismatch,
expiry, or reuse denies. The coding product offers no approvals and denies
these generic intents. Confirmation UI is cooperative, not enforcement.

Brokered secrets use exact manifest names:

```toml
[capabilities]
secrets = ["browser.api_token"]
```

```python
api_token = ext.get_secret("browser.api_token")
```

`get_secret(name, *, parent_request_id=...)` requires negotiated `secrets` and
active owner correlation. The manifest list is an exact allowlist, not environment
injection: duplicate-free names, at most 64 ASCII bytes, letter/underscore first,
then letters/digits/underscore/hyphen/dot. The broker gets manifest-bound identity,
full owner, parent, and name. Missing values/broker failures both give `-32004`
`secret is unavailable`. UTF-8 values cap at 64 KiB, checked by Python. Host
broker/writer buffers are best-effort wiped and never logged/persisted. Python
receives an immutable string, so end-to-end zeroization is impossible. Keep it
short-lived and out of logs, progress, results, metadata, and storage. The coding
product has no secret broker and offers no `secrets`.

Notifications and confirmations do not write stdout directly:

```python
ext.notify("Ready", level="success", title="local workflow")
if ext.confirm("Continue?", destructive=True):
    ...
```

For frozen API `0.1`, Python confirmation IDs are unsigned numeric IDs. API
`0.2` uses short `py:` strings to distinguish bidirectional cancellation from
numeric host IDs. Raw legacy clients may use unsigned numbers or strings up to
256 UTF-8 bytes. The [wire reference](../../docs/extensions/PROTOCOL-REFERENCE.md#2-extension-to-host-messages)
retains correlation, ID, and outstanding-request limits.

Stdout is reserved for JSON-RPC; `ext.log.info(...)` and other log levels emit
structured JSON stderr diagnostics. On `shutdown`, Python stops admission,
drains to a bounded deadline, cooperatively cancels the remainder, runs optional
`@ext.on_shutdown`, acknowledges, and exits. Stdin EOF uses the same bounded
drain before treating transport as lost. See [host shutdown and failure stages](../../docs/extensions/PROTOCOL-REFERENCE.md#18-shutdown).
