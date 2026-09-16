# Codex context windows: the deliberate 272K cap, its notice, and the opt-in override

This document covers how octet budgets an OpenAI Codex ("Sign in with ChatGPT")
OAuth session's context window, why the conservative cap exists, how a clamp is
reported, and how an operator can deliberately raise the window.

Implementation: `crates/octet-coding-agent/src/codex_context.rs` (policy, pure
resolution, notice). Bootstrap application:
`crates/octet-coding-agent/src/app/bootstrap.rs`
(`codex_context_override_from_env`, `codex_context_resolve_for_registration`,
`codex_context_record_note`, `record_codex_context_uncertainty`,
`register_openai_codex_with_notes`).

## 1. The cap is deliberate

`CODEX_CONTEXT_WINDOW_CAP = 272_000` is a product decision, not a defect:

* long-running Codex sessions were dropping their **websocket** connections as
  context grew;
* OpenAI **recommends** a 272K Codex context limit; and
* usage above 272K is **double-priced** (guidance: about 2x input / 1.5x output
  for the *whole* request, not only the excess).

octet therefore keeps the cap by default. `gpt-5.6-luna` is the one documented
exception: its conservative working window is 372K. The websocket drop-protection
work itself lives in `crates/octet-ai` and is owned separately; this document
only records the budgeting policy and the notice that references it.

## 2. Working window and entitlement table

| Model (`api_name`) | Working window octet budgets | Advertised/entitled ceiling |
| --- | --- | --- |
| `gpt-6-astra` | 272,000 | 872,000 |
| `gpt-5.4`, `codex-auto-review` | 272,000 | 1,000,000 |
| `gpt-5.6-luna` | 372,000 | 372,000 (family default) |
| `gpt-5.6-*` (sol, terra, ...) | 272,000 | 372,000 |
| any other/legacy Codex id | 272,000 | 272,000 |

* **Working window** = `working_context_window(model_id)`; what octet actually
  budgets requests against.
* **Advertised/entitled ceiling** = `entitled_max_context_window(model_id)`; the
  largest window the explicit override may ever request.
* The authenticated `/models` response remains authoritative for *advertised*
  limits: a smaller provider-advertised window stays authoritative, and
  `Pro`/`ProLite` (`ChatGptPlan::uses_max_context_window`) selects the larger
  advertised window before the working cap is applied. The cap therefore still
  applies on a Pro plan — that is intentional.
* `max_output_tokens` is always bounded by the effective `context_window`, and
  `gpt-6-astra` keeps its 128K output contract regardless of its input envelope.

Resolution is a pure function of its arguments:

```rust
octet_sdk::codex_context::resolve_codex_context_window(
    model_id,                        // e.g. "gpt-6-astra"
    tier,                            // CodexContextTier::{Default, Extended}
    discovered_default_context_window,
    discovered_max_context_window,
    advertised_max_output_tokens,    // Option<u64>
    user_override,                   // CodexContextOverride
) -> Result<CodexContextWindow, CodexContextWindowError>
```

`CodexContextTier::from_plan_entitlement(ChatGptPlan::uses_max_context_window())`
is the only conversion; `Extended` means "this account's plan activates the
model's larger advertised window".

## 3. The clamp is reported, not silent

`resolve_codex_context_window` returns
`CodexContextWindow::clamp: Option<CodexContextClamp>` whenever the deliberate cap
reduced the window below what the model advertises or the plan entitles. The
value is typed and bounded (a fixed template with numeric placeholders and the
model id), never an ad-hoc print:

```rust
pub struct CodexContextClamp {
    pub model_id: String,
    pub advertised_context_window: u64,
    pub effective_context_window: u64,
}
impl CodexContextClamp { pub fn message(&self) -> String; }
```

The message states the model id, the labelled advertised and effective windows,
and the reason (OpenAI's 272K recommendation, the double-priced cliff, and the
websocket-drop risk). It offers the in-app remedy first
(`--codex-context-window`, the model effort menu) and keeps
`OCTET_CODEX_CONTEXT_WINDOW` plus
`OCTET_CODEX_CONTEXT_WINDOW_ACKNOWLEDGE_COST_CLIFF=1` as the scriptable
alternative. Windows are printed with explicit labels (`advertised 872K,
entitled 872K, effective 272K`), never as three bare numbers.

A user-chosen *lower* window is not a clamp, and an override that was applied is
not a clamp either: only a deliberate reduction is reported.

### Emission: one note, never at startup

`codex_context_session_note(model_id, &window)` is the single user-facing note for
an *effective* Codex session model. It returns `None` unless that model's window is
reduced by the deliberate cap or above the 272K standard tier, and its wording is
plain user language: no internal API name, no operation id, and the effective
window is identical in the clamp and above-standard-tier variants (so the 372K
`gpt-5.6-luna` window never reads as a clamp to 272K).

Startup renders **nothing**: no note, no "starting extensions", no phase text.
Catalog construction only *records* notes (`CodexContextNotes`, one per model that
needs one); the recording is not an emission. Delivery is lazy and belongs to the
frontend that owns the transcript or an on-demand surface:

* interactive TUI / serve: the opened `App` carries the notes
  (`App::codex_context_notes`). Pull the note with
  `App::take_codex_context_note()` when the user can act on it — the first
  assistant turn after readiness — or read it without consuming via
  `App::codex_context_report()` from `/context`, `/status`, or `/telemetry`. Both
  accessors always use the effective model only, so a session on another provider
  shows no Codex note however many Codex models the catalog carries. These two
  calls are the frontend-owned delivery points (`modes/interactive.rs` and the
  `/context` / `/status` / `/telemetry` surfaces); no startup frame, catalog
  build, or extension lifecycle renders the note, and nothing in
  `app/bootstrap.rs` prints it;
* `Bootstrap::take_codex_context_note(&ModelId)` is the same latch available
  before `build_app`, kept for frontends that resolve a model without building the
  `App` yet;
* print / `--mode json` / `--mode rpc` launch resolution has no transcript to
  attach to, so the bounded line goes to stderr there.

The latch is once-per-session and survives `rebuild_app` (a delivered note stays
delivered, an undelivered note is carried to the new `App`). `shell.notice` and
`Bootstrap::codex_context_note` remain available for diagnostics, but no startup
frame consumes either. `CodexContextClampReporter::observe` remains available for a
frontend that tracks transitions after launch (a model switch mid-session reports
again, at most once per transition).

The same launch boundary records uncertainty: `build_app_with_runtime_manager`
calls `record_codex_context_uncertainty(&mut session, &model)` right after the
session is opened (and `rebuild_app` repeats it on a mid-session model switch), so
a route whose effective window is above 272K is durably `has_uncertain_usage`
before its first request. The decision is the Codex endpoint plus an effective
window above 272K, so a non-Codex provider with a large context window keeps exact
accounting. The record is sticky and written at most once per session.

## 4. The opt-in override

Requirements: opt-in only, bounded by the model's entitled ceiling, requiring the
Pro/ProLite entitlement for anything above the deliberate cap, and requiring an
explicit acknowledgement of the cost cliff and websocket risk.

### Interface

* CLI: `--codex-context-window <TOKENS>` with
  `--codex-context-window-acknowledge-cost-cliff` (parsed and validated by
  `crate::cli::parity::ParityOptions`, then published to the environment bridge by
  `install_codex_context_env` before bootstrap).
* Environment (used by bootstrap, so print/headless/TUI launches have the same
  lever): `OCTET_CODEX_CONTEXT_WINDOW` and
  `OCTET_CODEX_CONTEXT_WINDOW_ACKNOWLEDGE_COST_CLIFF=1`.
* API: `CodexContextOverride::{NONE, raising(tokens, acknowledged),
  parse(requested, acknowledged)}` then `resolve_codex_context_window`.

Accepted acknowledgement values are `1/true/yes/on`; `0/false/no/off` and empty
mean "not acknowledged". Anything else is an error. Parsing and resolution are
fail-closed: an unparseable or refused override leaves the deliberate cap in
force (with a warning) and never silently becomes a different window.

### Gates (all must pass)

| Request | Result |
| --- | --- |
| no override | deliberate cap, exact accounting |
| below `16_384` | refused: `OverrideBelowMinimum` |
| above the model's entitlement ceiling | refused: `OverrideAboveEntitlement` |
| above the working window on a non-entitled plan | refused: `OverrideRequiresEntitlement` |
| above the working window without the acknowledgement | refused: `OverrideRequiresAcknowledgement` |
| above the working window, entitled and acknowledged | granted, up to the entitlement |
| at or below the working window | granted (narrowing only) |

### Acknowledgement wording

Frontends must render `CODEX_CONTEXT_ACKNOWLEDGEMENT_WORDING` before accepting
the acknowledgement flag:

> I understand that a Codex request above 272K tokens is double-priced (about 2x
> input and 1.5x output for the whole request, not only the excess) and that
> oversized long-running sessions are more likely to drop the Codex websocket.

## 5. Above 272K, usage and cost are uncertain

Above the standard 272K tier the whole request is priced differently, so octet
must not present an exact-looking cost. `CodexContextWindow::has_uncertain_usage`
is `true` whenever the effective window exceeds 272K (including a granted
override, and `gpt-5.6-luna`'s documented 372K working window).
`CodexContextWindow::uncertain_usage_operation()` returns the operation id
`codex-context-above-272k` (`CODEX_ABOVE_STANDARD_TIER_OPERATION`) to pass to
`Session::record_usage_uncertainty(endpoint, model, operation)`, which keeps
`has_uncertain_usage` true and therefore keeps known-cost totals and hard
ceilings fail-closed.

The effective-model note for such a route says so in plain language ("this
session's usage is recorded as uncertain instead of an exact cost") without
naming the API or the operation id.

Wiring note: the record is appended by the launch boundary that owns the session
before the first request — `build_app_with_runtime_manager` calls
`record_codex_context_uncertainty`, and `rebuild_app` repeats it for a mid-session
model switch, using the operation id `uncertain_usage_operation()` returns. The
agent loop keeps its own attempt-scoped uncertainty records (failed/unknown
attempts) and treats `has_uncertain_usage` as sticky, so known-cost totals and hard
ceilings fail closed for the whole session.

## 6. It is client-side budgeting, not a wire field

`ModelLimits::context_window` controls when octet compacts and how large a request
octet is willing to send. The Codex request only carries `max_output_tokens`;
octet does not send a context-window envelope field, and no verified Codex wire
field exists for one. Raising this value therefore raises octet's own budget and
compaction threshold; provider-side enforcement is unchanged.

## 7. History

* `uses_max_context_window()` (Pro/ProLite entitlement signal) predates the cap.
* `d635c80b perf: cap Codex request context at 272K (#224)` introduced the flat
  272K cap deliberately, for the websocket/guidance/double-pricing reasons above,
  and bumped the Codex model cache schema to invalidate larger cached limits.
* `c29103fb ... Codex Luna context support (#398)` added the per-model
  `codex_context_window_cap` (372K for `gpt-5.6-luna`, 272K otherwise) and applied
  it on top of the plan gate, plus the `gpt-6-astra` 872K advertised envelope.
* The gap this change closes was **observability and consent**: the cap was
  silent (a session simply compacted at 272K with no explanation), there was no
  opt-in way to raise it, and nothing marked usage above 272K as uncertain. The
  budget values themselves are unchanged.
* The Codex model-cache schema is now version 7: entries carry the pre-cap
  backend default window (`default_context_window`) alongside the effective
  window, so an override and its notice are resolved exactly; version 6 caches
  are ignored and refreshed.

## 8. Startup visibility: nothing on screen, everything attributable off screen

Two properties are deliberately separated:

**The screen shows nothing.** The interactive frame, the pre-ready transcript, and
the extension/model startup path render no Codex note and no startup chatter. The
one Codex note a session owes is recorded during catalog construction and delivered
lazily (see §3), never by the startup frame.

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

## 9. Tests

* `crates/octet-coding-agent/tests/codex_context_window.rs` — cap holds per
  family on a Pro plan, non-entitled plans cannot exceed the cap, acknowledged
  Pro overrides reach 872K/1M, above-entitlement and unacknowledged requests fail
  closed, above-272K marks usage uncertain, output never exceeds the window, the
  notice fires once per transition and never without a clamp, the single session
  note is absent for an unreduced legacy route, luna's 372K is consistent in every
  variant, and no note names an internal API or operation id.
* `crates/octet-coding-agent/src/app/bootstrap.rs`
  (`codex_context_note_regression_tests`) — registration records one note per
  reduced model and none for an unreduced route (table-driven for Plus and Pro),
  a non-Codex effective model has no note while the catalog carries Codex models,
  the recorded note matches the effective resolution, and an above-272K route
  marks the session uncertain exactly once while a 272K route stays exact.
* `crates/octet-coding-agent/tests/parity_cli.rs`
  (`codex_context_notes_are_not_emitted_for_a_non_codex_session_or_per_turn`) —
  process boundary: with a synthetic Codex credential (so the catalog carries
  Codex models) and a two-turn non-Codex session, stderr contains no Codex note.
* `crates/octet-coding-agent/src/app/bootstrap/tests.rs` —
  `codex_context_tier_follows_the_plan_entitlement`,
  `codex_registration_keeps_the_deliberate_cap_and_reports_it_once`,
  `codex_registration_applies_only_an_acknowledged_entitled_override`.
* `crates/octet-coding-agent/src/codex_context.rs` — in-module unit tests for the
  gate matrix, parse fail-closed behaviour, and the reporter.
