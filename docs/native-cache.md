# Explicit provider-native prompt caches (unreleased)

Native cache maintenance is opt-in provider work, distinct from
[cache warming](cache-warming.md)'s billable generation requests. Normal client,
model and cache-owner construction never creates, probes, refreshes or deletes a
remote cache. There is no automatic Google cache creation in the CLI.

## Gemini Developer API: Rust embedding

`octet_ai::{GoogleCacheSession, GoogleCacheLimits, GoogleCachedContent}` provides
explicit create, refresh, delete and session-end close operations. The protocol
layer owns the `cachedContents` JSON, resource paths, authentication and TTL
rules. Only the direct `gemini` route at
`https://generativelanguage.googleapis.com/v1beta/` is admitted; Vertex and
compatible gateways do not inherit this API. See the provider's
[caching API contract](https://ai.google.dev/api/caching).

```rust,ignore
use std::{sync::Arc, time::Duration};
let mut caches = GoogleCacheSession::new(model.clone(), GoogleCacheLimits::default())?;
let result = async {
    // prefix is an explicitly selected Request ending with a user turn.
    let cache = caches.create(&prefix, Duration::from_secs(300)).await?;
    let options = HostRequestOptions {
        on_payload: Some(Arc::new(cache)),
        ..Default::default()
    };
    // full_request still contains complete context plus a suffix. Retain the
    // same model and endpoint used for cache creation.
    client.complete_with_host_options(
        &model, full_request, RequestOverrides::default(), options,
    ).await
}.await;
// Always await cleanup, even after failed creation or inference.
let cleanup = caches.close().await;
// Report cleanup separately; it must not prevent continuation without caching.
```

Call `refresh(&cache, ttl)` to PATCH only TTL, or `delete(&cache)` to remove one
resource. A 404 during deletion means already deleted. Refresh does not upload
content or invoke generation. Neither operation retries automatically.

The returned handle is a native payload hook: use it only with the model/endpoint
used to construct the owner. It checks model/provider/API identity and exact
encoded prefix values, including instructions and tools. It removes only that
prefix, including a cached part prefix inside a coalesced user content, then
sets `cachedContent`. An edited prefix, another identity, an expired handle or
an invalidated handle keeps the ordinary full request unchanged. A failed or
cancelled refresh disables the handle. Deletion, close and owner drop disable
all corresponding clones. The hook never creates or refreshes anything.

### Hard limits and cleanup

| Limit | Default | Allowed ceiling |
| --- | --- | --- |
| Owned entries, including uncertain creates | 4 | 8 |
| Requested TTL per create/refresh | 300 s | 3600 s |
| Total operation attempts per owner | 32 | 64 |
| Whole-operation deadline (auth, headers, body) | 10 s | 30 s |
| Encoded create body | 2 MiB | 2 MiB |
| Response body | 16 KiB | 16 KiB |

Limits must be positive; TTLs use whole seconds. The operation budget must
reserve at least two attempts per allowed entry. Create and refresh preserve a
delete-attempt reservation for every retained entry. Explicit deletion preserves
the session-end reservation too, so a failed delete cannot spend cleanup's slot.
There are no unbounded queues, retries, eviction loops or detached tasks.

The host must await `close()` when its session ends. Close invalidates handles
first and attempts every known resource once, even when another delete fails;
it is bounded by entries × operation deadline. Repeated completed close calls do
no network work. If close is cancelled, await it again; new create/refresh/delete
operations are refused once closing starts. Cancellation still consumes an
attempt; repeated cleanup cancellations can exhaust the finite operation budget.
Drop only invalidates local handles;
async network cleanup cannot safely run in Drop.

A cancelled, rejected or malformed create consumes an entry slot conservatively:
the provider may have accepted a resource whose name never reached the caller.
Unknown names cannot safely be deleted or evicted locally. Close reports this
uncertainty; finite provider TTL is the fallback after unknown creation, failed
cleanup, process death or host shutdown without awaited cleanup. A successful
close is not proof that an unknown resource was remotely deleted. Names are
strict resource identifiers, never provider-supplied URLs. Redirects are disabled;
errors omit response bodies, prefixes, keys and resource names.

Provider minimum token counts, availability, storage charges and quotas still
apply. Entry/TTL/operation bounds are not a USD budget. Native creation/storage
charges are not generation-time cache-write tokens, and this API does not invent
usage or silently add storage charges to the session's token ledger. Generation
continues to report Google's actual `cachedContentTokenCount` as cache reads,
with zero writes. If required, the host must separately reconcile storage bills.
No live cache-hit, billing or expiry claims follow from the offline fixtures.

## Anthropic, OpenAI and Codex

Existing `Request.cache_retention` remains the explicit Anthropic TTL choice:
`None` omits cache controls, `Short` uses the five-minute native default, and
`Long` requests one hour only when the selected route declares support. Otherwise
Long retains the short default. No arbitrary TTL or lifetime above one hour is
emitted. These request-local ephemeral caches have no named-resource delete API;
session end stops maintenance and provider TTL expires the entries. The existing
[warmer](cache-warming.md) bounds its horizon and spend separately.

OpenAI/Codex prompt caching uses route-declared keys and retention controls,
not a fabricated create/delete API. One-hour Anthropic writes remain a subset
of total writes. See [provider accounting and wire preservation](providers.md#prompt-caching).

All qualification here is offline. Live checks require separate approval and the
[maintainer checklist](maintainers/cache-live-qualification.md).
