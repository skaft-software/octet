# octet computer-use extension

This is the source-only, interface-first computer-use extension. It provides a
strict API 0.3 entry point plus typed policy and protocol boundaries. It does
**not** contain a desktop/browser backend, screenshot implementation, runtime
lifecycle, or transport client; those concerns remain host- or sibling-owned.

## Safety boundary

The extension is opt-in and declares no filesystem, process, or network
capability. Those manifest declarations are consent metadata, not OS
sandboxing. The standalone entry point has no policy evaluator or backend and
therefore denies every action.

A runtime owner may import `ComputerUseExtension` and inject both:

- a `PolicyGate` backed by the host's structured `policy/evaluate` adapter; and
- a local backend callback that receives an already-authorized action and its
  cancellation event.

The callback is not a provider or transport boundary. Provider credentials,
API keys, endpoint headers, leases, and transport authority are not part of the
context contract and must never be supplied to it.

Model text is never authority. The model supplies only an operation and
operation arguments. The host supplies the exact session, target, scope,
extension generation, frame generation, owner request, and trusted data
classification in `tool/call.context`.

## Tool contract

API 0.3 advertises one tool, `computer_use`, with:

```json
{"operation":"click","arguments":{}}
```

Supported operations are `observe`, `start`, `click`, `double_click`, `drag`,
`move`, `scroll`, `keypress`, `type`, `wait`, and `screenshot`. The operation
argument is classified into a typed capability/effect pair. It does not grant
permission.

Before dispatch, the policy layer binds the action to:

- exact target identity and browser origin when applicable;
- capability, effect, scope, session, owner, and extension generation;
- the SHA-256 digest of the private action arguments; and
- the current observation/frame generation and a short expiry.

Host approval is single-use and retry-bound. A stale, duplicated, malformed, or
mismatched decision fails closed. Coordinate actions must use the matching
fresh frame; backends must stop on failure, cancellation, takeover, or policy
boundary. Stop/takeover remains trusted host control.

Screenshots and other media are represented by host-owned opaque artifact
handles by the sibling artifact implementation, never inline data URLs or
base64 bodies here.

## Protocol

`main.py` implements canonical UTF-8 JSON followed by exactly one LF, strict
JSON-RPC 2.0 envelopes, API 0.3 negotiation, bounded identifiers/catalogs,
cooperative cancellation, and graceful shutdown. It selects only the required
API 0.3 foundation contract and does not silently negotiate optional provider,
lifecycle, migration, or dynamic-tool methods.

API 0.3 is the entry point's selected wire. API 0.2 remains supported by the
host generally, but this extension does not silently downgrade its canonical
contract.

## Development

The implementation is dependency-free Python and is intended to be exercised
by the extension test suite without starting a desktop or browser backend.
