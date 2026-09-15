# Codex context windows: the deliberate 272K cap, its notice, and the opt-in override

This document covers how octet budgets an OpenAI Codex ("Sign in with ChatGPT")
OAuth session's context window, why the conservative cap exists, how a clamp is
reported, and how an operator can deliberately raise the window.

Implementation: `crates/octet-coding-agent/src/codex_context.rs` (policy, pure
resolution, notice). Bootstrap application:
`crates/octet-coding-agent/src/app/bootstrap.rs`
(`codex_context_override_from_env`, `codex_context_resolve_for_registration`,
`codex_context_report`, `register_openai_codex`).

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

The message states the model id, the advertised/entitled window, the effective
window, and the reason (OpenAI's 272K recommendation, the double-priced cliff,
and the websocket-drop risk), and names the override lever.

Emission is once per transition via the reporter:

```rust
let mut reporter = CodexContextClampReporter::default();
if let Some(clamp) = reporter.observe(resolution.clamp.clone()) {
    // render clamp.message() once; repeated identical states report nothing
}
```

`observe(None)` reports nothing and re-arms the reporter, so a later transition
(model switch, plan change, or removing an override) reports again. Bootstrap
calls this from `codex_context_report` while registering Codex models, so each
clamped model is reported at most once per catalog construction — never per turn,
and never once per model registration pass.

A user-chosen *lower* window is not a clamp, and an override that was applied is
not a clamp either: only a deliberate reduction is reported.

## 4. The opt-in override

Requirements: opt-in only, bounded by the model's entitled ceiling, requiring the
Pro/ProLite entitlement for anything above the deliberate cap, and requiring an
explicit acknowledgement of the cost cliff and websocket risk.

### Interface

* CLI: `--codex-context-window <TOKENS>` with
  `--codex-context-window-acknowledge-cost-cliff` (parsed by
  `crate::cli::codex_context`, owned by the CLI worker).
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

Wiring note: the durable record must be appended by the agent/attempt path that
owns the provider attempt. octet's agent loop currently has no pre-attempt hook
for a route-level accounting decision, so the frontends that consume
`has_uncertain_usage` own that wiring; the flag and the bounded operation id are
exported for exactly that purpose.

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

## 8. Tests

* `crates/octet-coding-agent/tests/codex_context_window.rs` — cap holds per
  family on a Pro plan, non-entitled plans cannot exceed the cap, acknowledged
  Pro overrides reach 872K/1M, above-entitlement and unacknowledged requests fail
  closed, above-272K marks usage uncertain, output never exceeds the window, and
  the notice fires once per transition and never without a clamp.
* `crates/octet-coding-agent/src/app/bootstrap/tests.rs` —
  `codex_context_tier_follows_the_plan_entitlement`,
  `codex_registration_keeps_the_deliberate_cap_and_reports_it_once`,
  `codex_registration_applies_only_an_acknowledged_entitled_override`.
* `crates/octet-coding-agent/src/codex_context.rs` — in-module unit tests for the
  gate matrix, parse fail-closed behaviour, and the reporter.
