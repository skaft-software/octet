# Cache-warming advice example

This **API `0.4`** process adds a local spending preference; it does not perform
provider requests or enable cache warming. It returns `stop` when the estimated
refresh costs more than 5,000 microdollars ($0.005), and otherwise returns no
opinion so the host's economics or an earlier hook's action remain in force.

Install the source Python SDK from the repository root:

```console
python3 -m pip install ./sdk/python
```

Review the example, make its entrypoint executable on Unix, then explicitly
enable it with a source-built host:

```console
chmod +x examples/extensions/cache-warming/extension.py
octet --extension-dir ./examples/extensions --enable-extension cache-warming
```

Cache warming must separately be enabled in the host. An off, unsupported,
not-due, over-budget, cancelled, or replay-ineligible refresh cannot be enabled
by this hook. Capability declarations are not an OS sandbox, and discovery does
not run the process. The unpackaged example intentionally has no distribution
host-version pin.

The typed `@ext.cache_warming_decision` handler receives
`CacheWarmingDecisionPayload`: a `decision` containing phase (`streaming` or
`idle`), warm/miss costs, continuation probability, expected savings, economics
availability and host action, plus a secret-free `model` identifier. A two-
argument handler additionally receives the ordinary execution context with its
host-issued owner/instance/generation fence. No prompt or credentials are sent.

The host invokes registered hooks in order before every due refresh; the last
`warm` or `stop` opinion wins. No opinion, invalid output, failure, stale
generation or timeout falls back to the preceding decision. The process adapter
waits at most 200 ms, further limited by the host's aggregate hook budget and
original refresh deadline. Advice never changes billing ceilings, provider
eligibility, cancellation or replay safety. Keep handlers local and fast.

Generic `@ext.hook("cache_warming_decision")` handlers can instead return
`{"cache_warming_decision": "warm"}` or `{"cache_warming_decision": "stop"}`;
omit the field or use null for no opinion. Declaring the hook requires negotiating its matching feature,
which is offered only to API `0.4` manifests declaring the hook. This is not an
API `0.3` canonical-wire hook or Pi ABI compatibility.

Run the dependency-free stdio test (no provider or network calls):

```console
python3 examples/extensions/cache-warming/test_extension.py
```

See [the extension guide](../../../docs/extensions.md#cache-warming-decision-advice-api-04)
and [the Python SDK](../../../sdk/python/README.md#cache-warming-decision-advice-api-04).
