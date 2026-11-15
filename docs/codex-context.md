# Codex subscription context windows

octet uses the model's **provider default** context budget. There is no universal
272K cap, Pro/ProLite context gate, or cost-cliff acknowledgement.

## Sources and defaults

OpenAI's [Codex model guide](https://developers.openai.com/codex/models) and
[GPT-6.1 Sol model page](https://developers.openai.com/api/docs/models/gpt-6.1-sol)
state that **GPT-6.1 Sol defaults to 500,000 tokens in Codex and ChatGPT Work**.
The [Codex pricing guide](https://developers.openai.com/codex/pricing) explicitly
states that Sol usage drawn from a subscription has **no long-context surcharge**
and that API token prices must not be used to estimate included subscription tasks.
Those statements concern subscription usage, not the separate direct-API tariff.

Authenticated `/models` metadata takes precedence, including smaller limits.
The provider default and maximum are distinct: a maximum is not automatically
selected because a credential says Pro. Explicit positive metadata wins over
checked-in fallbacks; a model with only one advertised window uses that window
as both default and maximum. A default above an explicit maximum is normalized
to that maximum.

When neither window is advertised, or discovery is unavailable, reviewed
fallbacks are:

| Model (`api_name`) | Default | Maximum |
| --- | ---: | ---: |
| `gpt-6.1-sol`, `gpt-6.1-terra` | 500,000 | 872,000 |
| `gpt-6-astra`, `gpt-6-sol`, `gpt-6-luna` | 272,000 | 872,000 |
| `gpt-5.6-*`, `codex-auto-review` | 272,000 | 872,000 |
| `gpt-5.4` | 272,000 | 1,000,000 |
| other legacy/unknown models | 272,000 | 272,000 |

The [pinned bundled catalog](https://github.com/openai/codex/blob/126a0ee92ed2c5e73cd8a8f7e37563e8527552d7/codex-rs/models-manager/models.json)
provides the remaining fallbacks, including Terra's 500K default. It still lists
Sol at 272K; octet uses the explicit current public Sol default instead when no
account metadata is available. These fallbacks do not prove account availability,
reasoning capabilities, or subscription prices.

## Selecting a budget

```sh
octet --model codex/gpt-6.1-sol --codex-context-window 600000
# Equivalent launch-only environment setting:
OCTET_CODEX_CONTEXT_WINDOW=600000 octet --model codex/gpt-6.1-sol
```

The optional budget must be at least **16,384 tokens** and no larger than the
provider maximum for that model. It may raise or lower the default. Invalid
values are reported and registration keeps the provider default; they are never
silently clamped into a different requested value. The CLI checks the minimum;
model-specific maximum validation occurs against the inventory at registration.
Other providers are unaffected by this option.

The effort menu displays the **effective** budget and registered provider
**default** and **maximum**, not a reconstructed family/plan entitlement. When a
larger budget is available, the menu provides next-launch instructions; it does
not change the active catalog or persist configuration. Missing source metadata
does not produce an invented raise target.

The old `--codex-context-window-acknowledge-cost-cliff` flag was removed. Remove
it from scripts; the old acknowledgement environment variable is no longer read.
No acknowledgement is needed for a valid budget.

## Resolution and accounting

Implementation: `crates/octet-coding-agent/src/codex_context.rs` and registration
in `src/app/bootstrap.rs`. The SDK resolver is:

```rust
resolve_codex_context_window(
    model_id,
    discovered_default_context_window,
    discovered_max_context_window,
    advertised_max_output_tokens,
    CodexContextOverride::new(requested_tokens), // or NONE
)
```

`CodexContextOverride::parse` takes only the optional token-count string. The
retired tier, clamp, acknowledgement and above-272K uncertainty APIs are removed.
`ModelSpec::context_window_options` retains provider default/maximum separately
from the effective `ModelLimits::context_window`. Menus use those exact source
limits, so a live maximum smaller than a family fallback cannot be exceeded.
Output budgeting never exceeds the effective window; Astra retains its separate
128K output bound.

A large context window alone does **not** create an unknown-usage exposure.
Token receipts, genuinely unknown attempts and unpriced usage remain separate
accounting facts. Codex cost accounting is unchanged: the reviewed subscription
allowlist keeps its rates and long-context tiers, models without a reviewed rate
stay unpriced, and a public API quote never automatically prices an OAuth route.
Published credit rates are not converted into an invented per-token USD tariff.
Historical usage and uncertainty records are not rewritten or cleared.

Using the provider default is silent. An explicit override that changes it gets
one labelled note (provider default, maximum, effective), with no invented cost
cliff or context-based WebSocket warning. Catalog enumeration records rather
than prints that note. Frontends deliver it only for the effective session model,
at most once; non-Codex sessions do not receive it.

This is **client-side budgeting**, not a provider wire setting. It controls
compaction and request admission; provider-side enforcement is unchanged. Codex
Responses omits `max_output_tokens` and has no verified context-window wire field.
For a smaller working set, use the independent
[`compaction.max_active_tokens`](context.md) setting.

## Startup visibility: nothing on screen, everything attributable off screen

Two properties are deliberately separated:

**The screen shows nothing.** The interactive frame, the pre-ready transcript, and
the extension/model startup path render no Codex note and no startup chatter. Any
explicit-override note a session owes is recorded during catalog construction and delivered
lazily (see above), never by the startup frame.

**Latency stays attributable off screen.** Every startup boundary calls
`startup_phase("<name>")` (`crate::app::bootstrap::startup_phase`). It is silent by
default; with `OCTET_STARTUP_TRACE=1` it writes one line per boundary to stderr:

```
octet-startup: <phase> elapsed=<micros>us
```

Stable phase names, in startup order:

| Phase | Covers |
| --- | --- |
| `catalog.base` | embedded catalog plus the cheap local availability filter (no network) |
| `catalog.selected` | selected-route inventory wait for a non-Codex builtin route; with `catalog.base` before it, the delta is the wait readiness attributes to the user's own selection (skipped for a Codex route, which registers inside `catalog.codex`) |
| `catalog.codex` | selected-route Codex registration |
| `catalog.copilot` | selected-route Copilot registration |
| `catalog.fallback` | plan could not name the route; fleet catalog built |
| `catalog.enrich` | deferred (non-selected) provider inventories, never awaited by readiness |
| `codex.credentials` | credential load / bounded refresh for the selected route |
| `codex.inventory` | one bounded inventory request for the selected route |
| `bootstrap.ready` | catalog + session store + client ready |
| `session.resolve` | session selection (picker / resume provenance) |
| `session.replay` | reading the session's model/reasoning provenance |
| `extensions.provider-preflight` | temporary provider-only extension start |
| `extensions.prestart` | prestarted extension catalog sync |
| `extensions.activate` | `ExecutableExtensions` startup (already `join_all`, never serialized) |
| `app.build` | `App`/agent construction |
| `history.hydrate` | frontend transcript hydration |
| `frame.ready` | first interactive frame |

`codex.credentials` and `codex.inventory` are the two halves of the selected
route's wait: credential refresh has its own 60s HTTP deadline and a bounded
cross-process refresh-lock wait (`REFRESH_LOCK_WAIT`), and the inventory request
keeps its 10s deadline. The whole selected-route initialization additionally runs
under `CODEX_READINESS_ENVELOPE` (10s) through `run_route_readiness`, which signals
cancellation on timeout and never leaves an indefinitely blocked worker behind; a
timeout is a typed `RouteReadinessTimeout` that names the phase and the deadline.
Unrelated configured providers are not part of that envelope at all: readiness
names only the proven route (plus a configured compaction route), and everything
else is deferred to `Bootstrap::enrich_catalog`.

## Tests

- `tests/codex_context_window.rs`: documented defaults, source-limit precedence,
  overrides across 272K without consent, lower/upper bound validation, output
  budgeting, parsing and truthful override notes.
- Bootstrap tests: live/cache/fallback registration, all-plan default behavior,
  old-cache invalidation and subscription/API pricing separation.
- CLI and interactive tests: obsolete flag removal, minimum validation,
  live-limit menus and no surcharge/uncertainty copy.
- `octet-ai` tests: optional source-limit metadata round trip and validation.
