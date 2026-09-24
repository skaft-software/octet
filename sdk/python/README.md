# octet-extension-sdk

This checkout's source distribution is **0.7.6**, not a claim of SDK registry
publication. For version-matched published native assets and installation
evidence, see the [octet 0.7.6 release record](../../docs/releases/v0.7.6.md).

For new tools, `octet_extension.Extension` supports the current working-tree
**API `0.4`** feature-negotiated JSON-RPC wire, alongside retained `0.1`/`0.2`.
Use an exact `api_version="0.4"` constructor and matching manifest. This is
source support for the local 0.8.0 RC, not SDK registry or release qualification.
Extensions add bounded tools/integrations; the SDK does not grant host policy,
frontend ownership, or Pi execution parity.

Generated `octet_extension.api_v03` models and canonical validators remain live
for the distinct API `0.3` wire. They do not supply a canonical stdio process
runtime, and changing a version string does not translate a wire. The retained
[API `0.3` process example](../../examples/extensions/api-v03-minimal/README.md)
keeps its exact `=0.7.6` host pin. See the [current guide](../../docs/extensions.md)
and [versioned reference](../../docs/extensions/API-0.4-REFERENCE.md).

The dependency-free source package is named `octet-extension-sdk`; from a checkout:

```console
python3 -m pip install ./sdk/python
```

This installs the source package, not proof of installed-host qualification or
registry publication. Its source distribution version is `0.7.6`, independent
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
`0.1`/`0.2` examples and safety details; its older “new authoring uses 0.3”
statement is superseded by this README. API `0.4` reuses the feature-negotiated
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
