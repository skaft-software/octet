# Extension event bus (bounded, host-mediated)

API `0.3` has an optional `event_bus` service with a Rust process dispatcher and
coding-product binding. Its binding-scoped lifecycle contract is implemented in
the host, the generated contract and the Python SDK: every request and event
carries the host-issued `binding_id`, the host pushes `bus/lifecycle` control
notices, and the SDK owns a cancellable rebinding worker. Host bus unit tests,
two real Python process fixtures and product discovery/bus tests execute in the
current receipts. **A real active-session switch A→B→A with both surviving
processes, and fenced incoming requests across that switch, are still not
captured as product evidence**, so surviving-peer recovery is not claimed as
qualified behavior. See the [generated contract](API-0.3-REFERENCE.md) for exact
wire models and [extension parity](../parity/extensions.md) for remaining gates.

The coding product binds one bus to its active session's isolated API `0.3`
processes, after ordinary enablement, trust, source and process-policy checks.
Workspace-shared processes and legacy API `0.1`/`0.2` processes do not receive the
service. Generic hosts must explicitly supply `ExtensionRuntimeConfig.event_bus`;
without it, the capability and all `bus/*` methods are omitted and calls receive
canonical `unknown_method`. No bus grant changes persisted project trust.

## Wire and ownership

Select `event_bus` and the methods used; a subscriber must also select the
host-to-extension `bus/event` notification. The host derives identities from the
registered process, never from request parameters.

Every request below and every `bus/event` carries `binding_id`, an opaque
host-issued identity for one active bus incarnation. A peer captures the value
it was granted; a request whose captured value is not the current incarnation is
refused with `capability_mismatch` and never replayed under the new one.

1. `bus/declare` registers an immutable topic `bus.<owner>.<name>` and a bounded
   list of scalar `BusFieldSpec` fields. The owner must equal the host-derived
   manifest name. Duplicate declarations and foreign namespaces fail closed.
2. `bus/subscribe` / `bus/unsubscribe` use one exact, already-declared topic.
   There are no wildcard subscriptions and no direct peer addressing. A subscribe
   returns a typed result: `active` with the publisher instance/generation when a
   current declaration exists, or `pending`, which admits a bounded interest only.
   A pending interest is not an active subscription and delivers nothing until a
   later availability notice and a fresh subscribe acknowledgement.
3. `bus/publish` contains only `topic` and `payload`. The host validates the exact
   declared fields, assigns sequence and Unix-millisecond publication time, and
   atomically admits the event to all current subscribers' bounded writers.
4. `bus/event` carries `topic`, `publisher`, `publisher_instance_id`,
   `process_generation`, `sequence`, `published_at_ms`, and `payload`.
   These identity fields are inert provenance, not redeemable handles.

The host-to-extension `bus/lifecycle` notification is required of any peer that
selects `event_bus`. It reports either a replacement `binding` (new `binding_id`
and monotonic `binding_revision`) or a `topic_available` / `topic_unavailable`
transition with the exact topic, topic revision and publisher instance/generation.
Control notices share the same physical writer slots and credits as data: a peer
with no credit left is retired rather than silently missing a lifecycle change.

Sequences increase for the whole publisher process generation (and therefore
for each of its topics); session resets do not rewind that counter, while a
reload is a new generation with its own sequence. A replacement process must
redeclare; its subscribers must subscribe again. Unsubscribe, publisher removal,
subscriber shutdown and active-session replacement invalidate pending frames.
The session reset discards topics, subscriptions and the previous binding, not
extension-private memory. A trusted executable process is not an OS sandbox.

## Bounds and failure semantics

| Bound | Maximum |
| --- | ---: |
| Topics per session / attached process generations | 128 / 64 |
| Fields per topic / alternatives per enum | 24 / 32 |
| Exact-topic subscriptions per process | 16 |
| Topic / identifier bytes | 96 / 64 |
| String bytes / complete event-frame bytes (excluding LF) | 1024 / 8192 |
| Queued events / queued bytes per subscriber process | 64 / 256 KiB |
| Queued event age | 30 seconds |

Negotiated frame bounds and physical writer capacity can narrow these limits.
Queue credit is held until the frame is written or discarded, not just dequeued.
Session resets and publisher reloads do not refund pending subscriber credits.
All recipient slots are reserved before any publication is committed: pressure
returns `resource_exhausted`, does not partially fan out, and does not consume a
sequence. Successful publication acknowledges **admission**, not guaranteed peer
processing. Expired or invalidated queued frames are discarded before writing.
If expiry or invalidation interrupts an already-started write, the writer fails
closed and terminates that process rather than completing a stale partial frame.
Bytes already written cannot be retracted. Process loss is not replayed; no
automatic retry is implied by a lost acknowledgement.

Payload fields are only `string`, portable `integer`, `boolean`, or bounded
`enum`; no nested objects, arrays, unknown fields, omitted required fields, or
out-of-range integers are accepted. String and enum values are screened for
control characters, credentials/PEM/bearer-shaped values, e-mail/phone-shaped
values and private paths. Authority/credential/path-shaped field names are
rejected at declaration. Screening is deliberately conservative, not a PII
classifier or a proof about arbitrary encoded strings.

Malformed/unknown topics and payloads return `invalid_params`; foreign ownership
returns `capability_mismatch`; byte, subscription, registry and queue limits
return `resource_exhausted`. Errors use canonical API `0.3` code/message pairs.
A valid rejected request does not require killing an otherwise healthy process.

## SDK and evidence

`octet_extension.event_bus` contains the Python `HostEventBus` participant and a
deterministic reference kernel. No operation is attempted before the host has
delivered a binding notice through `accept_lifecycle`; without one, requests fail
closed as unbound. `declare` sends `bus/declare` before local registration,
`subscribe` requires a typed acknowledgement, publish timestamps and sequences
come from the host, and inbound events are validated against generated types,
topic ownership, binding and sequence fences. The request adapter is
`(method, params, cancelled)`: the serial reader never waits for an RPC response,
and one separate cancellable worker performs rebinding, so a replacement binding
clears the declaration/subscription/sequence ledger and re-establishes only the
bounded desired set. Missing or invalid lifecycle control fails the participant
closed instead of continuing on an apparently current stale ledger.

The SDK's legacy `Extension` runtime does **not** become an API `0.3` runtime;
wire these helpers to an ordinary current-API process loop. Recreating a helper
still is not a substitute for consuming `bus/lifecycle`, and no publication is
replayed: a peer that misses a notice is retired, not silently resynchronized.

The schema generates Rust, Python and TypeScript models, canonical fixtures and
the API reference. Regenerate/check with:

```console
python3 scripts/generate-extension-api-v03.py --check
PYTHONPATH=sdk/python python3 -m unittest discover -s sdk/python/tests
node sdk/typescript/tests/api_v03_conformance.mjs
```

Behavioral fixtures (Rust execution is parent-owned):

- `crates/octet-agent/src/extension_process/event_bus.rs`: atomic queue pressure,
  message/byte held-frame credit across reset/reload, generation/subscription/
  session/age fencing, blocked partial-write cancellation, and hostile fields.
- `crates/octet-agent/tests/extension_event_bus.rs`: two real Python processes,
  owner-only publication, typed refusals, ordered subscription delivery, reload,
  and separate-session/unavailable-service isolation.
- `crates/octet-coding-agent/src/extensions/bus_tests.rs`: actual product discovery
  and delivery, with no bus event in general host observations or durable session.
- `sdk/python/tests/test_event_bus.py`: bounded SDK/kernel/client regressions.

## Not claimed

No payload persistence, model/provider-stream delivery, telemetry, remote bus,
authority delegation, session steering, capability transfer, credential service,
or project-trust mutation. No Pi bridge compatibility is implied by an API `0.3`
service: a legacy bridge would need its own reviewed adapter and negotiation.
