START 2026-09-15T15:43:45Z ctx5 alive

## Step 1 — reconnaissance (READ ONLY)
- `2026-09-15T15:4xZ` Read `crates/octet-coding-agent/src/app/bootstrap.rs:4270-4300`
  (constants), `4530-4590` (`codex_model_context_limits`, `codex_context_window_cap`,
  `codex_context_window_for_plan`, `codex_model_limits`), `4420-4500`
  (`codex_models_from_response`), `4676-4930` (`fallback_codex_models`,
  `register_openai_codex`).
- Verified plan gate `ChatGptPlan::uses_max_context_window` is `pub(crate)` at
  `crates/octet-coding-agent/src/auth/codex/oauth.rs:85` and `ChatGptPlan` is
  `pub(crate)` (`auth/codex/mod.rs:18`) => the new public entry point must take a
  public tier enum, not the private plan type.
- `git show d635c80b` (pre-#224 `crates/ygg-coding-agent/src/app/bootstrap.rs`)
  shows the pre-regression body: `if Pro { max } else { default }` with NO cap.
- `git show c29103fb:...bootstrap.rs` shows #398 added
  `codex_context_window_cap` (luna 372K, else 272K) ON TOP of the plan gate.
- Wire-field check: `grep -rn context_window crates/octet-ai/src` => `context_window`
  is only `ModelLimits` (client-side budgeting/compaction); the only Codex request
  field is `max_output_tokens`. No context-window envelope field is sent.

## Step 2 — FRAMING CORRECTION RECEIVED (no edits to bootstrap yet)
Maintainer: the 272K cap is DELIBERATE (websocket stability, OpenAI guidance,
>272K is double-priced). Do NOT change the cap policy. Revised scope:
KEEP cap; add (a) typed/bounded once-per-transition clamp NOTICE, (b) opt-in
bounded override that requires Pro/ProLite + explicit acknowledgement above 272K,
(c) above-272K => uncertain usage.
Confirmed at the time of correction: only `docs/swarm-audit/EXECUTION-ctx5.md`
was modified by me; `git status` shows no source edits from ctx5. Nothing to revert.

## Step 3 — new module `src/codex_context.rs` + `lib.rs` registration
- `2026-09-15T15:5xZ` wrote `crates/octet-coding-agent/src/codex_context.rs`
  (NEW, 603 lines) and added `pub mod codex_context;` to
  `crates/octet-coding-agent/src/lib.rs` (next to `mod commands;`).
- Exported names for cli5/tui5 (STABLE):
  * `octet_sdk::codex_context::resolve_codex_context_window(model_id, tier,
    discovered_default_context_window, discovered_max_context_window,
    advertised_max_output_tokens, user_override) -> Result<CodexContextWindow,
    CodexContextWindowError>` — pure, no I/O.
  * `CodexContextTier::{Default,Extended}` + `from_plan_entitlement(bool)`
    (feed it `ChatGptPlan::uses_max_context_window()`).
  * `CodexContextOverride::{NONE, raising(tokens, acknowledged), parse(req, ack)}`
    + env `OCTET_CODEX_CONTEXT_WINDOW`,
    `OCTET_CODEX_CONTEXT_WINDOW_ACKNOWLEDGE_COST_CLIFF`.
  * `CodexContextClamp { model_id, advertised_context_window,
    effective_context_window }` + `::message()` (bounded fixed template).
  * `CodexContextClampReporter::observe(Option<CodexContextClamp>)` — fires exactly
    once per transition.
  * `CodexContextWindow { context_window, advertised_context_window,
    entitled_max_context_window, max_output_tokens, tier, override_applied,
    clamp, has_uncertain_usage }` + `::uncertain_usage_operation()`.
  * `CODEX_ABOVE_STANDARD_TIER_OPERATION = "codex-context-above-272k"` for
    `Session::record_usage_uncertainty`.
  * `CODEX_CONTEXT_ACKNOWLEDGEMENT_WORDING` — required wording (double-pricing
    2x/1.5x + websocket drop risk).
  * `working_context_window(id)` / `entitled_context_windows(id)` — the single
    source of truth for the deliberate cap (272K, luna 372K) and the per-family
    entitlement table (astra 872K, gpt-5.4/codex-auto-review 1M, gpt-5.6-* 372K,
    legacy 272K).
- COMMAND: `cargo check -p octet-coding-agent --lib`
  OBSERVED: FAILED before reaching my crate — `crates/octet-agent/src/tools/deferred.rs:280`
  `error[E0277]: the trait bound DeferredHandle: Eq is not satisfied` (another
  worker's in-flight edit to a file outside my paths). Not caused by ctx5.

## Step 4 — bootstrap wiring (my path only)
- `crates/octet-coding-agent/src/app/bootstrap.rs`:
  * constants block now points at `crate::codex_context` (single source of
    truth); `CODEX_MODEL_CACHE_VERSION` 6 -> 7.
  * `DiscoveredCodexModel` gained `default_context_window` (pre-cap backend
    default; `#[serde(default)]`, cache version gates it).
  * `codex_model_context_limits` delegates to
    `codex_context::entitled_context_windows`; new `codex_context_tier(plan)`.
  * `codex_models_from_response` / `fallback_codex_models` resolve through
    `resolve_codex_context_window` with `CodexContextOverride::NONE` (the cap
    policy is byte-for-byte the same as before).
  * `register_openai_codex` reads `OCTET_CODEX_CONTEXT_WINDOW` +
    `OCTET_CODEX_CONTEXT_WINDOW_ACKNOWLEDGE_COST_CLIFF` (fail-closed), re-resolves
    each model, reports the clamp once per transition, and names the uncertain
    accounting obligation when an override is applied.
  * cache validation requires `default_context_window` 1..=max.
- `crates/octet-coding-agent/src/app/bootstrap/tests.rs`: `DiscoveredCodexModel`
  literal + cache-version assert updated; three new tests added.
- `tests/codex_context_window.rs` (NEW, required target) added.
- `docs/codex-context.md` (NEW) added.
- COMMAND (standalone type-check + unit tests of the new module, possible while
  other workers keep the workspace transiently broken):
  `rustc --edition 2021 --test -o /tmp/codex_ctx_test crates/octet-coding-agent/src/codex_context.rs && /tmp/codex_ctx_test`
  OBSERVED: first run 1 failure (`has_uncertain_usage` wrongly asserted true at
  272K) -> fixed; re-run 4 passed.

## Step 5 — REQUIRED test target RUN (observed)
COMMAND: `cargo test -p octet-coding-agent --test codex_context_window`
OBSERVED (first run): 7 passed, 2 failed — both failures were my test
expectations, not the policy: (a) a Plus override of 500_000 for gpt-5.6-sol hits
the 372K entitlement bound before the plan gate; (b) the clamp notice belongs to
the entitled path (Default tier for astra is already 272K, so nothing is clamped
there). Fixed the two tests.
COMMAND (rerun): `cargo test -p octet-coding-agent --test codex_context_window`
OBSERVED:
running 9 tests
test an_override_above_entitlement_fails_closed ... ok
test acknowledged_pro_override_reaches_the_entitled_maximum ... ok
test clamp_notice_fires_once_per_transition_and_never_without_a_clamp ... ok
test above_the_standard_tier_accounting_is_uncertain ... ok
test output_is_never_larger_than_the_context_window ... ok
test a_plus_plan_cannot_exceed_the_deliberate_cap_even_with_an_override ... ok
test deliberate_cap_holds_for_every_family_even_on_a_pro_plan ... ok
test override_parsing_is_opt_in_and_fail_closed ... ok
test the_deliberate_cap_and_its_luna_exception_are_documented_values ... ok
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
Also: `cargo check -p octet-coding-agent --lib` => Finished (green) after
cleaning one unused-local warning of mine; remaining warnings are other workers'
in-flight files.

* START 2026-09-15T16:28:33Z ctx6 alive

## ctx6 (successor) — adopted and finishing (step 6)
- Adopted ctx5's work: `codex_context.rs` (606 lines), `tests/codex_context_window.rs`
  (9 tests), `docs/codex-context.md`, bootstrap wiring all present. Verified
  unchanged policy: working window still 272K (luna 372K); entitlement table
  astra 872K / gpt-5.4+codex-auto-review 1M / gpt-5.6-* 372K / legacy 272K.
- VERIFIED `cargo test -p octet-coding-agent --test codex_context_window`
  => 9 passed; 0 failed (output pasted in ctx6 final step).
- GAP FOUND (`crates/octet-coding-agent/src/app/bootstrap.rs::codex_context_report`):
  the above-272K accounting note fired only on `override_applied`, so the
  documented 372K `gpt-5.6-luna` route (and any future above-tier window) left
  accounting silently exact. FIXED: the note now fires on
  `CodexContextWindow::uncertain_usage_operation()` / `has_uncertain_usage`, i.e.
  on every route whose effective window exceeds 272K, and it names the
  `Session::record_usage_uncertainty` operation id.
