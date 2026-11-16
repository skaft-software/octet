# Opt-in live cache qualification

**NOT RUN.** Offline fixtures establish encoding, accounting and bounds, not
provider acceptance, cache hits, storage billing, expiry or real savings. This
checklist never authorizes a live call. Obtain separate owner approval for the
provider, account, exact model, maximum requests, maximum USD/storage exposure,
retention and deletion verification before starting. Do not use credentials from
another worker or include them in receipts.

## Preflight

- [ ] Record the exact source commit, provider route, model, account scope,
  reasoning, service tier and configured entry/TTL/operation/deadline bounds.
- [ ] Establish actual model cache minimums, quotas and storage pricing from the
  provider contract. Decline if exposure cannot fit the approved budget.
- [ ] Use disposable nonsensitive fixture text, a dedicated session and a safe
  trace location. Record only payload-free usage, sizes and request counts.
- [ ] Prepare an awaited session-end cleanup path and a manual provider-console
  fallback. A local future cancellation is not proof of remote cancellation.

## Generation/accounting matrix

- [ ] Anthropic: exercise None/Short/Long on declared-support routes; inspect
  final `cache_control` TTLs, 5m/1h creation subsets, read counters, integer cost,
  footer ratio and persisted/reopened session totals. Count the 1h subset once.
- [ ] OpenAI Chat and Responses: capture terminal prompt totals and nested
  cached-token counts; reconcile disjoint input/read/write buckets and costs.
- [ ] Codex: use account-advertised models and route controls, not public OpenAI
  retention/pricing assumptions. Exercise a tool continuation with hooks active.
- [ ] Google: reconcile `promptTokenCount`, `cachedContentTokenCount`, candidate
  and thought tokens against final usage and persisted session/footer displays.
  Do not invent cache writes from storage creation or call a byte match a hit.
- [ ] For each approved route, compare the final encoded stable instructions,
  schemas and first-turn bytes across at least eight growing turns with identity
  and control-editing payload hooks. Intentional prefix edits should invalidate
  only the corresponding prefix. Compare after hooks, not only canonical JSON.

## Explicit lifecycle matrix

- [ ] Google: create one resource, invoke generation using its checked prefix,
  refresh TTL with PATCH only, delete it, then verify remote absence separately.
- [ ] Exercise configured entry and operation limits: local rejection sends no
  request, and cleanup's reserved delete attempts remain available.
- [ ] End a session with two known resources and verify both deletions. Test an
  approved failure/cancellation case; retain uncertain exposure and verify finite
  TTL expiry when the returned resource name is unavailable.
- [ ] Verify expired/deleted/closed local handles send ordinary full context and
  never trigger hidden maintenance. Do not replay ambiguous accepted inference
  just to test a cache; obtain explicit approval for any replacement generation.
- [ ] Anthropic: verify the selected 5m/1h expiry and that session end stops
  warming. There is no named-resource deletion to simulate.
- [ ] Record native storage charges separately from generation token usage.
  Confirm actual resource expiry and bill settlement, not just HTTP success.

## Receipt

List every command, request count, provider-reported token bucket, known cost,
uncertainty and cleanup result. Separate local byte preservation, remote cache
hits, token savings, total bill and latency; none proves the others. Mark each
missing route/credential/account step **NOT RUN** and retain incomplete status.
See [explicit cache operations](../native-cache.md) for the offline contract.
