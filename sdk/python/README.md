# octet-extension-sdk

This checkout's source distribution is **0.7.6**, not a claim of SDK registry
publication. For version-matched published native assets and installation
evidence, see the [octet 0.7.6 release record](../../docs/releases/v0.7.6.md).

Python has generated API `0.3` contract models and canonical-wire validators in
`octet_extension.api_v03`. Use the [current extension guide](../../docs/extensions.md)
and [generated API reference](../../docs/extensions/API-0.3-REFERENCE.md) for new
authoring. Exact fields, optional/null distinctions, bounds, errors, and
availability come from that contract.

**The `octet_extension.Extension` runtime supports legacy API `0.1`/`0.2`, not
a complete API `0.3` extension runtime.** Generated types do not supply API `0.3`
stdio dispatch, request scheduling, cancellation, or process lifecycle. Do not
change a legacy manifest or constructor to `0.3` and expect an upgrade. The
qualified dependency-free API `0.3` process example is
[api-v03-minimal](../../examples/extensions/api-v03-minimal/README.md); it is a
reference loop, not a replacement for this retained legacy runtime.

The dependency-free source package is named `octet-extension-sdk`; from a checkout:

```console
python3 -m pip install ./sdk/python
```

This installs the source package, not proof of current-API runtime parity or
registry publication. Its source distribution version is `0.7.6`, independent
of the extension API version. Native octet publication does not publish the SDK
to PyPI. Imports and wire names
use `octet_extension`, `octet_version`, `requires_octet`, and `OCTET_*`, with no
aliases for earlier first-party names.

For application embedding rather than extension authoring, see
[native-host protocol `1`](../../docs/sdk.md), a separate interface.

## Bounded event bus

`octet_extension.event_bus` carries the bounded, extension-scoped bus contract:
typed topics (`bus.<owner>.<name>`), bounded queues, owner-only publish,
fail-closed validation of unknown topics, forbidden authority-shaped fields, and
PII/secret/private-path values. The module holds the enforcement kernel
(`EventBusKernel`, `TopicRegistry`, `BoundedQueue`, `validate_payload`) and the
extension-side participant (`HostEventBus`).

API `0.3` hosts can now bind a session-isolated `event_bus`; the coding product
binds it only to isolated current-API processes after its existing startup gates.
`HostEventBus.declare` sends `bus/declare`; publication identity, generation and
time are host-owned. A missing or unselected method still fails closed. This is
not a legacy `Extension` runtime upgrade. See the [bus contract](../../docs/extensions/event-bus.md)
for bounds, Rust process/product fixtures and verification status.

## Legacy runtime reference

Existing API `0.1`/`0.2` implementations can use the
[complete legacy runtime reference](legacy-runtime.md). It retains decorators,
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
