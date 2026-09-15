# Pinned metadata repair: bounded source review

**Status: source review only; verification UNRUN.** This document is not a live
provider qualification, a benchmark result, or a discovery test target. No new
integration target was created.

## Scope and evidence

A supplied pre-repair `octet-coding-agent` library diagnostic identified seven
failures in the `pinned_metadata` and `third_party_gpt_6_astra` bootstrap tests.
That diagnostic is historical input, not a result for the repaired source.

The bounded review covered the existing bootstrap discovery/registration code,
its library tests, provider declarations and source-owned reasoning profiles,
catalog validation, and the checked-in snapshot provenance. No commands, tests,
builds, formatter, Git, network requests, or credential/native qualification
were run during this repair.

## Contract retained

- The pinned models.dev supplement supplies missing **display names/pricing
  only**, for actual inventory-returned built-in routes. It supplies no limits,
  modalities, tools, structured output, or reasoning controls.
- Endpoint assertions, including false, unknown, null, and malformed values,
  are not replaced with snapshot optimism. Exact endpoint choices/defaults
  narrow only a compatible provider/protocol contract.
- Documented declaration-owned sparse defaults remain independent of the
  snapshot. Sparse direct `deepseek-flash` keeps 128K context / 64K output and
  its native Off/low/high/max contract; legacy V4 retains separate 1M/384K and
  Off/high/xhigh defaults. Direct DeepSeek pricing remains unknown unless
  explicitly configured.
- Third-party Astra does not inherit direct OpenAI features. The OpenRouter
  fixture now distinguishes missing capability assertions from endpoint-positive
  image/tools/structured-output and exact low/high reasoning metadata.
- A native declaration-owned effort-budget table must fit strictly below the
  effective output ceiling. Discovery retains the model and its limits but
  drops an incompatible reasoning capability, rather than enlarging output or
  inventing a budget table. Compatible native codecs/options remain unchanged.
- Existing configured/custom precedence, raw provider-cache format, route
  selection, and account-authoritative Codex behavior are unchanged.

## Source changes and test coverage

[Bootstrap registration](../../crates/octet-coding-agent/src/app/bootstrap.rs)
filters incompatible native budget tables in both generic and Messages discovery
registration. The OpenRouter incomplete-route comment now explicitly requires an
endpoint-supplied completion ceiling.

[Existing bootstrap library tests](../../crates/octet-coding-agent/src/app/bootstrap/tests.rs)
reconcile all seven named failures with the display/pricing-only contract. Added
coverage directly checks display-only merging (including explicit name blockers),
endpoint-positive capabilities, and native budget/output boundaries: equality,
just above the maximum budget, small limits, sparse defaults, and context clamps.
Provider/protocol scoping, Cerebras defaults, independent limit leaves, DeepSeek
alias contracts, exact reasoning choices/defaults, and native codec identity
remain explicit assertions.

The [provider guide](../providers.md) and
[snapshot source record](../../crates/octet-ai/models/SOURCES.md) state the current
contract. The coordinator reconciled the source record's historical enrichment
prose during integration; no snapshots, generator or pricing data were changed.

## Outstanding verification

| Check | Status |
| --- | --- |
| Coding-agent library tests filtered by `pinned_metadata` | UNRUN |
| Coding-agent library test filtered by `third_party_gpt_6_astra` | UNRUN |
| Compilation, formatting, and full library regression checks | UNRUN |
| Live provider, credential, or native integration qualification | UNRUN |

Only source inspection and edit-diff review were performed. These changes still
need authorized validation; no passing test or live acceptance claim is made.
