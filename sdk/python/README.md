# octet-extension-sdk

This SDK's source distribution is **0.8.0**. Native assets and installation
evidence are recorded in the [0.8.0 release notes](../../docs/releases/v0.8.0.md).
Native publication does not publish SDK packages to PyPI or npm.

For new tools, `octet_extension.Extension` supports the current working-tree
**API `0.4`** feature-negotiated JSON-RPC wire, alongside retained `0.1`/`0.2`.
Use an exact `api_version="0.4"` constructor and matching manifest. This is
source SDK support for octet 0.8.0, not a claim of SDK registry availability.
Extensions add bounded tools/integrations; the SDK does not grant host policy,
frontend ownership, or Pi execution parity.

Generated `octet_extension.api_v03` models and canonical validators remain live
for the distinct API `0.3` wire. They do not supply a canonical stdio process
runtime, and changing a version string does not translate a wire. The retained
[API `0.3` process example](../../examples/extensions/api-v03-minimal/README.md)
keeps its exact `=0.8.0` host pin. See the [current guide](../../docs/extensions.md)
and [versioned reference](../../docs/extensions/API-0.4-REFERENCE.md).

The dependency-free source package is named `octet-extension-sdk`; from a checkout:

```console
python3 -m pip install ./sdk/python
```

This installs the source package, not proof of installed-host qualification or
registry publication. Its source distribution version is `0.8.0`, independent
of the extension API version. Native octet publication does not publish the SDK
to PyPI. Imports and wire names
use `octet_extension`, `octet_version`, `requires_octet`, and `OCTET_*`, with no
aliases for earlier first-party names.

For application embedding rather than extension authoring, see
[native-host protocol `1`](../../docs/sdk.md), a separate interface.

## Minimal API 0.4 tool

Install the source SDK into the Python environment used by the entrypoint.
Create `my-extensions/hello-tools/extension.toml`:

```toml
name = "hello-tools"
version = "0.1.0"
api_version = "0.4"

[entrypoint]
command = "python3"
args = ["/absolute/path/to/my-extensions/hello-tools/extension.py"]

[capabilities]
filesystem = "none"
process = false
network = false

[contributes]
tools = ["hello"]
```

Set the script path to the actual absolute path (the child cwd is the workspace,
not its package). Beside the manifest, save `extension.py`:

```python
from octet_extension import Extension

ext = Extension(api_version="0.4", max_concurrent_requests=1)

@ext.tool(name="hello", description="Return a local greeting")
def hello(args):
    ext.cancellation.raise_if_cancelled()
    return "Hello from octet."

ext.run()
```

With a source-built host supporting API `0.4`, review and explicitly enable it:

```console
octet --extension-dir ./my-extensions --enable-extension hello-tools
```

Check `/extensions status`, then ask the model to call `hello`; expect
`Hello from octet.` in the tool result. Enablement is explicit. Full-access
policy supplies trust without persisting a grant; `--safe-mode` never starts
executable extensions. Capability declarations are not an OS sandbox. Use a
reviewed isolated environment and keep stdout exclusively for protocol.

This unpackaged recipe intentionally has no `requires_octet` pin. Distributed
bundles must supply an exact host-version requirement and matching artifacts.
The host authoring smoke must also exercise cancellation during bounded work
and clean shutdown, not just load or inspect generated types. Longer handlers
must poll cancellation between effects; cancellation never means rollback.

## Typed tools and diagnostics (API 0.4)

`typed_tool` is additive: existing `@ext.tool(parameters=..., output_schema=...)`
and dictionary handlers retain their behavior. A synchronous typed handler takes
one annotated dataclass (and optionally the existing ambient context), and returns
an annotated value with an explicit model-facing summary:

```python
from dataclasses import dataclass
from typing import Optional
from octet_extension import Extension

ext = Extension(api_version="0.4")

@dataclass
class Record:
    name: str
    enabled: bool
    samples: list[float]
    note: Optional[str] = None

@ext.typed_tool(name="echo", description="Echo a typed record",
                summary=lambda value: "Echoed typed record.")
def echo(value: Record) -> Record:
    ext.cancellation.raise_if_cancelled()
    return value

ext.run()
```

Both schemas and codecs are generated at registration. Successful output is
`structured_content`, not JSON disguised as text. Supported values are UTF-8
strings, booleans, portable signed integers (±(2^53−1)), finite floats,
homogeneous `list[T]`, nonrecursive dataclasses, `Optional[T]`, string `Enum`
and string `Literal` values. Integers supplied to float fields become floats;
booleans never become numbers. Records are closed. `Optional` permits explicit
null, not omission: absence requires an explicit dataclass default. Defaults
(including factories) are evaluated/validated at registration and decoded afresh
for each omitted field; null never substitutes a non-null default. Unsupported
maps, tuples, recursive types, general unions, unannotated types and async
handlers fail registration rather than degrading to unchecked dictionaries.

Schemas are bounded to 64 KiB; values to 256 KiB, depth 32 and 16,384 nodes;
individual strings to 128 KiB. Explicit summaries are nonempty plain text of at
most 4096 UTF-8 bytes. Input mismatch refuses before handler entry; invalid typed
output is not published as success. Existing cancellation and negotiated
`ext.progress` are reused; progress is ephemeral, not a result/data stream.
No asynchronous runtime is introduced.

For diagnostics, annotate the return `TypedResult[Record]` (omit the decorator's
`summary=` callback) and return, for example:

```python
from octet_extension import Diagnostic, TypedResult

# Inside an annotated typed handler:
return TypedResult(None, "Validation failed.", is_error=True, diagnostics=(
    Diagnostic("error", "solver.nonconvergent", "Operating point did not converge."),
))
```

An error may omit its structured value. `DiagnosticLocation`, `DiagnosticSpan`,
`WorkspaceSource`, `BlobSource`, `ArtifactSource`, `DiagnosticRelated`,
`DiagnosticFix`, `DiagnosticEdit` and `DiagnosticAttachment` describe optional
locations, suggested revision-checked edits and existing authorized references.
Never invent an unavailable source or revision. `tool_result(..., diagnostics=...)`
or `with_diagnostics(result, diagnostics)` also works with the explicit API.

Diagnostics use reserved `metadata.octet_diagnostics_v1`; malformed raw reserved
metadata is rejected too. The SDK appends the shared plain-text projection (first
eight diagnostics, at most 4096 UTF-8 bytes) without duplicating it. All nested
records are closed and bounded; exact wire shapes/limits are in the
[shared profile](../conformance/README.md). References and suggested fixes do not
grant access or effect authority. Host result admission remains authoritative.

Run `PYTHONPATH=sdk/python python3 -m unittest discover -s sdk/python/tests -v`
from the checkout. The [typed host smoke](../../examples/extensions/python-single-file/TYPED.md)
uses a real Python subprocess through production Rust `ExtensionProcess`, not a
synthetic host. The single-file package generator still recognizes only explicit
`@ext.tool` declarations; schema inference is not a packaging feature.

## Native resources and immutable bulk (API 0.4)

Declare a nominal native class once, then use `Resource[T]` in fixed dataclass
fields. The SDK generates its closed wire schema, canonical exclusive input and
output operation slots, and the tool's stable operation ID (tool name by default).
No schema/pointer/nominal-name duplication is needed:

```python
from dataclasses import dataclass
from octet_extension import Extension, Resource

ext = Extension(api_version="0.4")

@ext.resource_type("example.Counter.v1")
class Counter:
    def __init__(self, value):
        self.n = value

@dataclass
class Create:
    value: int

@dataclass
class Created:
    counter: Resource[Counter]

@ext.typed_tool(name="create", description="Create native counter",
                summary=lambda result: "Created counter.")
def create(args: Create) -> Created:
    return Created(ext.export(Counter(args.value)))

@dataclass
class Increment:
    counter: Resource[Counter]

@ext.typed_tool(name="increment", description="Increment native counter",
                receiver="/counter", summary=lambda result: "Incremented counter.")
def increment(args: Increment) -> int:
    args.counter.value.n += 1
    return args.counter.value.n
```

`operation_id=` optionally overrides the generated ID. `receiver=` is optional
presentation metadata naming one generated input slot, never routing/authority.
Resource slots cannot appear inside lists, Optional/unions, persistent defaults,
or at the output root; use fixed record fields. Copies of a `Resource` alias the
same object. Exporting the same native object twice is refused.

For explicit cleanup, declare `@ext.resource_type("example.Counter.v1",
dispose=lambda native: native.close())`. Cleanup runs on the existing request
executor after host retirement, with all batch references invalidated first.
A failed disposer never restores resolution. Callers must bound their cleanup;
the host owns finite cleanup deadlines and reports completed/failed/unknown
separately from retirement. Cancellation is **not** a disposal or execution fence.
Only the host decides publication, pins, busy-release refusal and retirement.
If `ext.export(native)` raises, ownership did not transfer: the author must clean
up native state. Successful registration is provisional until full result admission.

For immutable bytes, annotate a field with `BlobRef` and let the SDK generate the
closed SHA-256 descriptor schema. `BlobRef` annotations automatically opt into
`bulk_objects_v1`; dictionary-only tools may call `ext.enable_bulk()` before run.
Inside a handler:

```python
from octet_extension import BlobRef

@dataclass
class Waveform:
    data: BlobRef
    samples: int

# Return this from an appropriately annotated handler:
waveform = Waveform(ext.bulk.publish_bytes(binary_bytes,
                        media_type="application/octet-stream"), samples)

# In a later handler, after the host admitted the original success:
with ext.bulk.read(waveform.data, max_bytes=160 * 1024) as reader:
    payload = reader.read()
```

`publish_bytes(data: bytes, *, media_type="application/octet-stream")` writes
only through parent-correlated tickets and computes SHA-256; it returns a
provisional `BlobRef`, never a locator. `read(reference, *, max_bytes)` requires a
finite caller bound, snapshots verified bytes into bounded memory, and yields a
binary reader. It closes the OS file before yielding and closes the snapshot/
releases the lease even on parser errors or cancellation. This convenience API
is not an unbounded streaming interface. Raw bytes/base64/transport locators
never enter tool results. Descriptor knowledge grants no authority; every read
requires host authorization. Retention/durable recovery remain host-owned.

Resource/operation/bulk features are negotiated only when used. Existing tools
retain their feature set. Missing features, malformed limits or unavailable
`local-file.v1` fail closed. Resource limits are 256 records and 32 registrations
per parent. Bulk honors the host's finite limits (default 256 MiB/object,
512 MiB/session, 8 tickets and 32 leases/generation, 256 objects/session),
independent of existing frame and media-artifact limits. Secure local-file
helpers require POSIX `dir_fd`, `O_DIRECTORY`, and `O_NOFOLLOW`; unsupported
platforms refuse that profile rather than following unchecked paths.

## Vision compaction strategy (API 0.4)

A manifest may declare `hooks = ["compaction_strategy"]` without declaring a
tool. The host offers the matching `compaction_strategy` feature only on API
`0.4`; select it in `supported_features` along with required
`request_cancellation` and `content_parts`. `@ext.hook("compaction_strategy")`
receives a bounded `{model_id, text}` payload and returns
`{"compaction_frames": [base64_png, ...]}`. Octet selects the hook only for
local compaction on vision routes and validates all frames before persisting a
checkpoint. See [octet-snap-compact](../../extensions/octet-snap-compact/README.md)
for a full renderer using the source SDK.

## Cache-warming decision advice (API 0.4)

Declare `hooks = ["cache_warming_decision"]` in the API `0.4` manifest. The
host offers the matching optional feature only to that declaration. The default
SDK feature set supports it; an explicit `supported_features` list must include
it alongside required `request_cancellation` and `content_parts`.

```python
from typing import Optional
from octet_extension import CacheWarmingAction, CacheWarmingDecisionPayload, Extension

ext = Extension(api_version="0.4")

@ext.cache_warming_decision
def advise(payload: CacheWarmingDecisionPayload) -> Optional[CacheWarmingAction]:
    if payload["decision"]["warm_cost_microdollars"] > 5_000:
        return "stop"
    return None  # Preserve the host's economics or an earlier hook's action.
```

The payload has `decision` (phase, warm/miss cost, continuation probability,
signed expected savings, economics availability and host action) and `model`.
Phases are `streaming` and `idle`; monetary fields use microdollars. A two-
argument handler receives the ordinary owner-fenced execution context as its
second argument. Prompt text, provider transport and credentials are absent.

A generic `@ext.hook("cache_warming_decision")` handler may return
`cache_warming_decision("warm")`, `cache_warming_decision("stop")`, or no
opinion (`None`, `{}`, or a null action). The helper builds the typed wire result
`{"cache_warming_decision": "warm"}`. Invalid actions fail closed.

Before every due refresh the host invokes registered hooks in order; the last
returned action wins. Failure, invalid output, timeout, stale generation or no
opinion preserves the preceding decision. Each process wait is capped at 200 ms
and is also subject to the host's aggregate hook budget and original refresh
deadline. Advice never enables warming by itself or changes eligibility,
spending/attempt limits, cancellation or replay safety. Keep handlers fast and
local; provider I/O remains host-owned. API `0.1`/`0.2`/`0.3` cannot register this
hook. See the [runnable local example](../../examples/extensions/cache-warming/README.md)
and [host contract](../../docs/extensions.md#cache-warming-decision-advice-api-04).

## Bounded event bus

`octet_extension.event_bus` carries the bounded, extension-scoped bus contract:
typed topics (`bus.<owner>.<name>`), bounded queues, owner-only publish,
fail-closed validation of unknown topics, forbidden authority-shaped fields, and
PII/secret/private-path values. The module holds the enforcement kernel
(`EventBusKernel`, `TopicRegistry`, `BoundedQueue`, `validate_payload`) and the
extension-side participant (`HostEventBus`).

API `0.3` hosts can now bind a session-isolated `event_bus`; the coding product
binds it only to isolated canonical API `0.3` processes after its existing startup gates.
`HostEventBus.declare` sends `bus/declare`; publication identity, generation and
time are host-owned. A missing or unselected method still fails closed. This is
not a canonical-wire upgrade to the feature-negotiated `Extension` runtime.
See the [bus contract](../../docs/extensions/event-bus.md) for bounds, Rust
process/product fixtures and verification status.

## Legacy runtime reference

The [complete legacy runtime reference](legacy-runtime.md) preserves API
`0.1`/`0.2` examples and safety details. API `0.4` reuses the feature-negotiated
runtime, not the canonical `0.3` wire. The reference retains decorators,
handshake, scheduling, logging, all host-request helpers, ownership, security,
media, cancellation, and shutdown behavior. The [wire reference](../../docs/extensions/PROTOCOL-REFERENCE.md)
retains exact legacy messages and limits. Those APIs are not API `0.3` aliases.

These topic anchors preserve links from the former combined SDK README:

- <a id="contribution-points"></a>[Contribution points](legacy-runtime.md#contribution-points)
- <a id="api-02-negotiation-and-scheduling"></a>[API `0.2` negotiation and scheduling](legacy-runtime.md#api-02-negotiation-and-scheduling)
- <a id="semantic-presentation-snapshots"></a>[Semantic presentation snapshots](legacy-runtime.md#semantic-presentation-snapshots)
- <a id="dynamic-tool-catalogs"></a>[Dynamic tool catalogs](legacy-runtime.md#dynamic-tool-catalogs)
- <a id="child-model-sessions"></a>[Child model sessions](legacy-runtime.md#child-model-sessions)
- <a id="cancellation-and-progress"></a>[Cancellation and progress](legacy-runtime.md#cancellation-and-progress)
- <a id="structured-results-and-artifacts"></a>[Structured results and artifacts](legacy-runtime.md#structured-results-and-artifacts)
- <a id="parent-correlation-and-lifecycle"></a>[Parent correlation, input, lifecycle, policy, secrets, and shutdown](legacy-runtime.md#parent-correlation-and-lifecycle)

## Host-owned worker model routing

On the feature-negotiated wire, `agent_sessions` plus
`agent_model_selection_v1` enables `list_agent_models(query=None, limit=50)`
and `spawn_agent(..., model_selection={"provider": "…", "model": "…", "reasoning": "low"})`.
Discovery is owner-correlated, caps query text at 128 UTF-8 bytes and limits at
1–100, and returns `{models: [...], truncated: bool}` without credentials.
Omit `model_selection` to inherit exactly; omitted selection keys mean `inherit`.
Reasoning accepts `inherit`, `off`, `on`, `minimal`, `low`, `medium`, `high`,
`xhigh`, `max`, and `ultra`; `on` covers binary/always-on models.
Only the host admits configured routes and normalizes reasoning; explicit routing
never silently falls back. Spawn/list records retain requested selection and
`policy.resolved_model` effective provider/model plus serialized `ReasoningConfig`.
