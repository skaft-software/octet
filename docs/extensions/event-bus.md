# Extension event bus (bounded, host-mediated)

Status: **SDK primitive and host contract landed; host mediation NOT
implemented.** No extension can publish or subscribe today: the host has no
`event_bus` capability and no `bus/*` method, so `HostEventBus` calls fail closed
with the host's `unknown_method`. The row is recorded as `Partial` in
[docs/parity/extensions.md](../parity/extensions.md), with the exact remaining
work listed below.

Why a host-mediated bus: extensions are separate processes. They cannot share an
in-process queue, and they must never address each other directly. A bus that let
one extension hand another a capability, a handle, or a private path would be an
authority-escalation channel, so the design keeps the host as the only mediator
and the only identity authority.

## What exists now

`octet_extension.event_bus` (dependency-free, Python 3.9+) provides:

| Piece | Purpose |
| --- | --- |
| `BusLimits` | Bounded, validated limits: message bytes, payload fields/depth, string bytes, queue messages/bytes, subscriptions, topic bytes/segments, identifier bytes, message age, drain bound. Limits can only be lowered; values above the internal ceiling are refused. |
| `FieldSpec` / `TopicSpec` | Typed topic declarations (`string`, `integer`, `boolean`, `enum`) with per-field byte bounds and optional min/max. |
| `TopicRegistry` | Unique, owner-scoped declarations. Duplicate topics, forbidden field names, and malformed names fail closed. |
| `validate_topic` / `validate_payload` | The enforcement kernel: strict `bus.<owner>.<name>` syntax, exact field match, no unknown fields, no forbidden shapes, no PII/secret/private-path values, no control characters, bounded size and depth. |
| `BoundedQueue` | Bounded, ordered queue. A full queue raises `resource_exhausted`; it never silently drops, reorders, or overwrites. |
| `EventBusKernel` | Deterministic reference semantics for the host: declare, subscribe, publish (owner-only), per-`(extension, topic)` delivery, monotonic per-publisher sequence, age-based expiry at delivery. No I/O, no filesystem, no authority. |
| `HostEventBus` | Extension-side participant. Outbound `bus/publish`, `bus/subscribe`, `bus/unsubscribe` through the SDK host-request API; inbound `bus/event` validated with the same kernel rules before the extension sees it. |

Topic names are `bus.<owner>.<name>` where both segments are lowercase
`[a-z0-9][a-z0-9_-]*` and bounded. Only the owner may publish to its topic; any
extension may subscribe to another's topic. Subscribing is not a capability, and
a delivery carries only the topic, publisher identity, sequence, timestamp, and
validated payload.

## Fail-closed behavior

| Situation | Result |
| --- | --- |
| Unknown/malformed/foreign-prefix topic | `invalid_params` (`-32602`), nothing queued |
| Topic not declared in the registry | `invalid_params` (`unknown_topic`) |
| Publisher is not the topic owner | `capability_mismatch` (`-32011`, `foreign_topic`) |
| Subscriber reads without a subscription | `capability_mismatch` (`-32011`, `not_subscribed`) |
| Unknown field, wrong type, missing required field, enum miss, min/max miss | `invalid_params` |
| Authority/credential/private-shaped field name | `invalid_params` (`forbidden_field`) |
| E-mail, token/bearer/PEM-shaped, phone-shaped, absolute/home path, or control character in a string | `invalid_params` (`pii_detected`, `private_path`, `control_character`) |
| Message, field, string, depth, subscription, or queue bound exceeded | `resource_exhausted` (`-32012`) |
| Stale or duplicate inbound sequence | `invalid_params` (`stale_sequence`), delivery refused |
| Host rejects the call (for example `unknown_method`) | the `RpcError` propagates; nothing is swallowed or retried silently |

PII screening is a bounded deny-list heuristic, not a classifier. The host
implementation must apply at least these rules; a topic author who needs a
path-like or credential-like value must not put it on the bus at all.

## Host contract still required

The schema is the source of truth; the Rust host and both SDK bindings are
generated from it (`python3 scripts/generate-extension-api-v03.py`). Adding the
bus therefore means, in this order:

1. `protocol/extension-api-v0.3.schema.json`: add the `event_bus` capability
   alongside `theme_selection` (`:280` is the capability anchor, `:541` the
   theme method anchor) and the methods
   `bus/publish`, `bus/subscribe`, `bus/unsubscribe` (extension → host) and
   `bus/event` (host → extension notification, `required: false`, `available:
   false` until the host service is safely bound).
2. `crates/octet-agent/src/extension_api_v03.rs`: `CAPABILITY_SPECS` (`:80`) and
   `METHOD_SPECS` (`:106`); then regenerate the Python and TypeScript bindings
   and commit the regenerated `sdk/python/octet_extension/api_v03.py` and
   `sdk/typescript/src/api_v03.*`.
3. Host runtime: a per-session bus service that owns the registry and queues,
   applies the kernel semantics above, stamps the publisher identity and
   generation, and delivers only to subscribers. It must not persist payloads,
   expose them to the model or provider stream, or let a delivery carry a
   capability, handle, or trust change. Queue pressure must answer
   `resource_exhausted` rather than drop.
4. Tests: a Rust behavioral fixture mirroring
   `sdk/python/tests/test_event_bus.py` (owner-only publish, per-extension
   delivery, bounded queue, unknown topic, forbidden field, PII value, stale
   sequence) plus one end-to-end two-extension fixture through a real host.

Until (1)–(4) land, `HostEventBus.subscribe`/`publish` are usable only against
an injected request callable, and any real call surfaces the host error. That is
the intended fail-closed state, not a degraded success.

## Not claimed

- No cross-extension authority escalation of any kind: a bus message cannot
  carry a capability grant, a resource handle, a credential, a file path, or a
  project-trust change, and no delivery can start or steer a session.
- No persistence, no cross-session or cross-workspace delivery, no LAN/remote
  bus, and no delivery to a client that is not a subscribed extension instance
  of the same host.
- No telemetry: payload bytes are not written to telemetry, logs, or session
  records.
- Nothing here changes the persisted project-trust, OAuth/credential,
  clipboard-image, rg/fd download, or chord/CBOR/unix-socket gates.
