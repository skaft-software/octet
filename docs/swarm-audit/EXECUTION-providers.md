# Provider execution receipt

Base: `df5a7e80` plus this worker's uncommitted diff. Shared worktree; no commits,
branch changes, alternate target directory or live provider requests.

## Scope / evidence

- #424: bounded endpoint self-description, never models.dev capability authority.
  Existing snapshot remains display/pricing only; declared routes and codecs remain
  the authority for protocol intersections and native reasoning encodings.
- #245: reproduce and repair native Mistral URL/EOF classification at source;
  retain all existing fixture assertions. Native CLI presets remain a separate
  unimplemented surface, not claimed from codec repair.
- Verify #244/#246/#248/#249/#250/#252/#173 with focused deterministic tests.
  Pi inventory remains a decision ledger, not exact 0.84.4 or live parity proof.

## Observed results (incremental)

- Initial inspection: HEAD `df5a7e80`; owned paths initially clean. `df -h .`
  reported 38 GiB available; existing `target/` is the only build target.
- Read full provider/design docs and relevant linked snapshot provenance,
  discovery/thinking, Pi compatibility, Mistral and both Copilot candidate records.
  Historical FAILURES.md reports Mistral 14/16.
- Reproduced `cargo test --locked -p octet-ai --test mistral_current`: exit 101,
  14 passed / 2 failed, exactly the URL and missing-finish assertions recorded
  in FAILURES.md (before changes).

- After source repairs: `cargo test --locked -p octet-ai --test mistral_current`
  exit 0, **16/16 passed**. Assertions unchanged. Conversations enforces HTTPS or
  literal loopback HTTP before credential resolution; native unterminated streams
  emit annotated `MissingFinish` before generic EOF handling. No POST replay.
- `cargo test --locked -p octet-coding-agent --lib pinned_metadata`: exit 0,
  **13 passed**. Existing snapshot display/pricing-only tests remain green.
  Build emitted pre-existing unused/dead-code warnings and a shared-tree
  unfulfilled rescan lint expectation; no warning cleanup attempted.

## Parent-requested bootstrap failure diagnosis

- Re-ran `cargo test --locked -p octet-coding-agent --lib
  unknown_api_03_last_initial_provider_model_preflights_restarts_and_reloads_with_fresh_routes`:
  exit 101, 0 passed / 1 failed, same `runtime startup Launch` as parent's full run.
- A temporary direct-launch probe in owned provider tests exposed the actual
  child stderr: Node `ERR_MODULE_NOT_FOUND`, missing
  `<temporary pi-provider extension root>/semantic_ui.mjs`, imported by
  `extensions/octet-pi-compat/bridge.mjs`. Direct `ExtensionProcess::start` reports
  `Closed("extension stdout closed")`; runtime maps this to `Launch`.
- Root cause is the bridge selecting its **module dependency** relative to
  `OCTET_EXTENSION_DIR` (the fixture's launcher directory), although bridge.mjs
  itself is passed as an absolute path in the repository. Not a provider
  discovery/bootstrap capability regression or an assertion/timing problem.
  Needed outside-owned hook: Pi bridge owner should resolve bundled sibling
  modules relative to the bridge module URL (or supply a distinct validated
  source-module directory), preserving staged-entrypoint support. Parent's
  existing fixture must remain unchanged as a regression. Temporary probe was
  removed; no bridge or fixture edit made by this worker.

## Focused verification matrix

- `cargo test --locked -p octet-ai --lib --test provider_discovery --test mistral_current --test google_current --test bedrock_current --test client_stream`: exit 0.
  - Running unittests src/lib.rs (target/debug/deps/octet_ai-69051c8013db57f1)
  - test result: ok. 310 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.95s
  - Running tests/bedrock_current.rs (target/debug/deps/bedrock_current-5c97f545fba9dc72)
  - test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s
  - Running tests/client_stream.rs (target/debug/deps/client_stream-fd287e5976ca70e0)
  - test result: ok. 37 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.55s
  - Running tests/google_current.rs (target/debug/deps/google_current-21a8f340a45a923c)
  - test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
  - Running tests/mistral_current.rs (target/debug/deps/mistral_current-6a3bb2f857b23f98)
  - test result: ok. 16 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.11s
  - Running tests/provider_discovery.rs (target/debug/deps/provider_discovery-95bed366f5915c8d)
  - test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

- `cargo test --locked -p octet-coding-agent --test provider_contract --test vertex_current --test bedrock_auth_current --test copilot_current --test copilot_host_current`: exit 0.
  - Running tests/bedrock_auth_current.rs (target/debug/deps/bedrock_auth_current-ffebc45f4f2fe492)
  - test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
  - Running tests/copilot_current.rs (target/debug/deps/copilot_current-e815a97a4e405e99)
  - test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
  - Running tests/copilot_host_current.rs (target/debug/deps/copilot_host_current-389d0d7a6641ea4b)
  - test result: ok. 25 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.43s
  - Running tests/provider_contract.rs (target/debug/deps/provider_contract-38b5622e6c105ea0)
  - test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
  - Running tests/vertex_current.rs (target/debug/deps/vertex_current-70022a58aac6d586)
  - test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

- `cargo test --locked -p octet-coding-agent --lib providers::`: exit 0. test result: ok. 41 passed; 0 failed; 0 ignored; 0 measured; 1230 filtered out; finished in 0.03s

- `cargo test --locked -p octet-coding-agent --lib provider_self_description_tests`: exit 0. test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 1266 filtered out; finished in 0.00s

- `cargo test --locked -p octet-coding-agent --lib custom_model`: exit 0. test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 1265 filtered out; finished in 0.02s

- `cargo test --locked -p octet-coding-agent --lib third_party_gpt_6_astra`: exit 0. test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1270 filtered out; finished in 0.00s

- `cargo test --locked -p octet-coding-agent --lib mistral_conversations_discovery_does_not_invent_reasoning_controls`: exit 0. test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1270 filtered out; finished in 0.00s

- `cargo test --locked -p octet-agent --test agent_run openai_lifecycle_feedback_is_forwarded_but_not_persisted`: exit 0. test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 135 filtered out; finished in 0.04s

- Final diff check: `cargo test --locked -p octet-ai --test mistral_current --test provider_discovery`: exit 0.     Finished `test` profile [unoptimized + debuginfo] target(s) in 1.41s; test result: ok. 16 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.11s; test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

- Final diff check: `cargo test --locked -p octet-coding-agent --lib provider_self_description_tests`: exit 0.     Finished `test` profile [unoptimized + debuginfo] target(s) in 17.56s; test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 1266 filtered out; finished in 0.02s

- Final diff check: `cargo check --locked -p octet-ai -p octet-coding-agent --all-targets`: exit 0.     Finished `dev` profile [unoptimized + debuginfo] target(s) in 6.34s


## Final outcome and remaining boundaries

| Row | Observed verification / boundary |
| --- | --- |
| #424 | Implemented opt-in v1 self-description on existing Chat/Responses discovery and custom startup; 4 core + 5 bootstrap tests pass; 13 snapshot-contract tests pass. Endpoint/model/codec provenance is host-assigned; no snapshot operational metadata is imported. |
| #245 | Both demonstrated failures repaired at source; unchanged `mistral_current` 16/16 passes. Native preset/discovery exposure and broader Conversations features remain outside this repair. |
| #244 | Google codec 2/2 and declaration tests pass; bundled Gemini inventory is static, not newly implemented live Google model discovery. |
| #246 | Bedrock codec 5/5, auth integration 2/2, and provider-unit credential-chain coverage pass. |
| #248 | Vertex declaration 1/1 plus ADC unit coverage in `providers::` passes. |
| #249 | Copilot provider 8/8, host 25/25 and provider-unit lifecycle tests pass; no live OAuth or full protocol/inventory parity claim. |
| #250 | Workers AI declaration/base-URL and request-route fixtures pass in 41/41 provider unit tests. |
| #252 | Pinned Pi provider decisions and routed fixtures pass in 41/41 provider unit tests; still decision-complete, not exact 0.84.4/live parity evidence. |
| #173 | AI stream suite 37/37 and agent lifecycle test 1/1 pass; endpoint opt-in remains required, with no cold-start probe or 503 replay. |

Final all-target check for `octet-ai` and `octet-coding-agent` passes. New Rust
files pass scoped `rustfmt --check`; owned diff passes `git diff --check`.
Existing-file formatting was limited to new hunks, not global reformatting.
No unrelated shared changes were reverted. Last observed free space: 28 GiB.

Hooks outside ownership:

- Parent/Pi owner: fix the diagnosed bridge helper-base regression above, then
  rerun the unchanged bootstrap integration fixture. Its failure is independent
  of self-description/Mistral. No fixture weakening or production bootstrap
  workaround was applied.
- Guided setup (`crates/octet-coding-agent/src/provider_setup.rs:937`) uses its separate model
  parser: consuming the v1 object there needs an explicit hook to the same core
  decoder with host-selected provenance and existing override rules. Not changed.
- `docs/provider-thinking.md` still contains historical snapshot-reasoning
  enrichment wording contradicting the authoritative display/pricing-only
  contract. `docs/qualification/mistral-current-candidate.md` and Copilot candidate
  records retain historical UNRUN markers; this receipt supplies current focused
  results without claiming native/live acceptance. These docs were not owned.

No credentials, remote inference, installed-host acceptance, complete workspace
regression pass or release qualification is claimed. No session/artifact handle
beyond the shared diff, tool receipts and this file was exposed to this worker.

Source snapshot fingerprint (ordered path + NUL + bytes + NUL, receipt excluded):
`eb58af236df3a8b68ca54dd9932d31f6c1df1206525e39937728c3d7b4ac421f`. Base is `df5a7e80`; files are the nine source/test/doc
paths listed below plus this execution receipt.

- `crates/octet-ai/src/client.rs`
- `crates/octet-ai/src/lib.rs`
- `crates/octet-ai/src/protocol/mistral_conversations.rs`
- `crates/octet-ai/src/discovery.rs`
- `crates/octet-ai/tests/provider_discovery.rs`
- `crates/octet-coding-agent/src/app/bootstrap.rs`
- `crates/octet-coding-agent/src/providers/self_description_tests.rs`
- `docs/providers.md`
- `docs/design/octet-ai.md`

## AI_PATHS_RELEASED

Per root's sequencing request, providers has finished all active AI edits and
releases **`crates/octet-ai/**` except `src/discovery.rs` and
`tests/provider_discovery.rs`** to the codec-depth worker. No further writes to
released paths by this worker. Existing Mistral URL/EOF fixes and the public
`discovery` export are preserved. The focused passing results above precede this
handoff; they are not claims about subsequent shared edits.

Providers retains coding-agent providers/bootstrap, declarations, generator,
provider tests and assigned docs. Per-request core options and any other required
client/type/codec changes will be reported rather than implemented across the
released boundary. Bootstrap Launch remains the separately diagnosed Pi bridge
helper-resolution blocker described above; no test weakening was applied.

## providers3 continuation (rows 1a/1b/1d)

Base `df5a7e80`; shared dirty worktree. Adopting providers2's receipt above
(rows #424/#245/#244/#246/#248/#249/#250/#252/#173 already verified). This
section tracks the parity ledger rows assigned to providers.

### Adoption of partial work (2026-09-15T15:16Z)

- `git diff --stat -- <owned paths>` showed providers2 left: `crates/octet-ai/src/catalog.rs`
  (+1: reject `deferred_tool_loading` in `validate_model_spec`), `docs/providers.md`
  (+64: self-description + native Mistral sections), untracked
  `providers/self_description_tests.rs` (wired at `bootstrap.rs:6410`),
  and untracked `providers/conditional_inventory_tests.rs` (**not wired**).
- `cargo check -p octet-ai -p octet-coding-agent`: octet-ai ok; octet-coding-agent
  blocked by an unrelated in-flight crate: `sexy-tui-rs` E0599/E0004
  (`TextEditAction` non-exhaustive match, `TakeWhile::next_back`) and, earlier,
  `octet-agent` `FindTool: Tool`. Those are other workers' paths; not touched.
  Shared-tree build is not stable enough to compile `octet-coding-agent` yet.

### Landed: octet-ai declarations module — rows 1b.2 / 1b.5 / 1b.6 (2026-09-15T15:2xZ)

- New `crates/octet-ai/src/declarations/mod.rs` (615 lines) + `pub mod declarations;`
  and re-exports in `crates/octet-ai/src/lib.rs`.
- Typed, validated, fail-closed preset data: `ModelPreset` (`sampling_params`,
  `headers`, `vllm_priority`, `supports_max_output_tokens`,
  `thinking_token_budget_field`, `chat_template_args`/`chat_template_kwargs`),
  `ChatTemplateValue`/`ChatTemplateVariable` with exact upstream `{ "$var": ... }`
  semantics (`thinking.enabled` bool, `thinking.budget` number,
  `thinking.effort` level-mapped, `omitWhenOff`), `ThinkingFormat` incl.
  `string-thinking` and `qwen-chat-template`, `ProviderCredentialPreset`
  (ANTHROPIC_AUTH_TOKEN/ANTHROPIC_OAUTH_TOKEN/GOOGLE_CLOUD_API_KEY ordering +
  bearer presentation marker), bounded `RequestOverrides`.
- Command: `cargo test -p octet-ai --lib declarations::`
  Observed: `test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 314 filtered out`.
  (First run had 1 failure — duplicate-var check wrongly counted bearer aliases;
  fixed, re-ran, 7/7.)
- GAP: this is declared plumbing only. No codec/host consumes `ModelPreset` yet
  (codec-depth owns codec files). Integration of samplingParams/headers/$var into
  the OpenAI-Chat codec and host wiring of `RequestOverrides` remain the gap.

### Landed: 1b.4 wiring + 1a.1 declarations + test (2026-09-15T15:3xZ)

- `crates/octet-coding-agent/src/app/bootstrap.rs` tail: added
  `#[path = "../providers/conditional_inventory_tests.rs"] mod
  provider_conditional_inventory_tests;` so providers2's orphaned 1b.4 test is
  compiled. The 1b.4 mechanism itself (`ProviderInventoryResponse`, `etag`,
  `checked_at`, `If-None-Match`, scoped validators) was already in bootstrap.rs.
- `crates/octet-coding-agent/src/providers/declarations.json`: added 5
  OpenAI-compatible presets — `baseten` (BASETEN_API_KEY,
  https://inference.baseten.co/v1/), `qwen-token-plan` + `qwen-token-plan-individual`
  (QWEN_TOKEN_PLAN_API_KEY), `qwen-token-plan-cn` (QWEN_TOKEN_PLAN_CN_API_KEY),
  `zai-coding-cn` (ZAI_CODING_CN_API_KEY, https://open.bigmodel.cn/api/coding/paas/v4/).
  Each: openai_chat route, bearer, openai_models/all discovery, static none,
  inventory required, pricing reference.
- `crates/octet-coding-agent/src/providers/contract.rs`: added unit test
  `token_plan_and_coding_provider_declarations_are_declared` asserting ids,
  labels, base URLs, env vars, single Chat route, bearer, discovery, pricing.
- Commands/observed:
  - `python3 -m json.tool declarations.json` → valid JSON.
  - `touch build.rs && cargo build -p octet-coding-agent` → build.rs ran clean
    (no `provider declaration`/manifest error), i.e. the new declarations pass
    `validate_provider`; the only failure is an **unrelated** in-flight crate
    error `E0004 commands::Command::Fast(_) not covered` in
    `src/modes/interactive.rs:4039` (another worker's path).
  - `cargo check -p octet-coding-agent` → same single external error.
- BLOCKED (external, not mine): the coding-agent lib cannot be linked while
  `modes/interactive.rs` does not handle `Command::Fast`, so the two new tests
  (1a.1 unit test, 1b.4 integration test) are not yet executed. Re-polling.
- GAP for 1a.1: upstream ships generated static model catalogs for these five
  providers (models.dev-derived `providers/data/*.json`), which are absent from
  the read-only reference checkout. This lands the declared provider + route +
  discovery; exact static model lists remain a follow-up needing models.dev
  generation. No upstream TypeScript is vendored.

### Landed: 1b.3 proxy resolution core (2026-09-15T15:4xZ)

- New `crates/octet-ai/src/declarations/proxy.rs` (268 lines) reproducing
  upstream `pi` `resolveHttpProxyUrlForTarget` / `shouldProxyHostname` as pure
  logic: `resolve_http_proxy` (scheme proxy → `all_proxy`, scheme defaulting,
  http/https only), `no_proxy_excludes` (exact + `.domain` + `*.domain` root AND
  subdomain; optional `:port`; lone `*` disables; `*` inside a list is a no-op),
  `proxy_env_value` (lower-case before upper-case). Re-exported from lib.rs.
- Command: `cargo test -p octet-ai --lib declarations::`
  Observed: `test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 321 filtered out`.
  (One test expectation was corrected to match upstream exactly: a lone `*`
  disables proxying, a `*` entry inside a list is skipped.)
- Command: `cargo test -p octet-ai --lib`
  Observed: `test result: ok. 333 passed; 0 failed; 0 ignored; 0 measured`.
- Both new files pass `rustfmt --edition 2021 --check`.
- GAP: `client.rs` (`reqwest::Client::builder` at :1891) is codec-depth-owned;
  the `reqwest::Proxy`/`NoProxy` seam that would call this resolver is reported,
  not changed.

### Landed: 1b.6 Anthropic bearer-token aliases (2026-09-15T15:5xZ)

- `crates/octet-coding-agent/src/providers/auth.rs`: added
  `bearer_token_variable` (matches `ANTHROPIC_AUTH_TOKEN` / `ANTHROPIC_OAUTH_TOKEN`,
  keyed on the credential variable, never a provider name). `environment_auth`
  and `environment_discovery_headers` now emit `Authorization: Bearer` for those
  variables even on the Anthropic route's `api_key_header` presentation.
- `crates/octet-coding-agent/src/providers/declarations.json`: Anthropic
  variables reordered to `[ANTHROPIC_AUTH_TOKEN, ANTHROPIC_OAUTH_TOKEN,
  ANTHROPIC_API_KEY]`.
- Tests added: `bearer_token_aliases_override_the_route_api_key_header`,
  `anthropic_declaration_lists_bearer_aliases_before_api_key`.
- Command: `cargo test -p octet-coding-agent --lib providers::auth`
  Observed: `test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 1301 filtered out`.
- Earlier in the same window: `cargo test -p octet-coding-agent --lib providers::`
  `42 passed`; `token_plan_and_coding_provider_declarations_are_declared` `1 passed`;
  `provider_conditional_inventory_tests` `5 passed`.
- GAP (1b.6): `GOOGLE_CLOUD_API_KEY` for `google-vertex` is not landed. octet's
  vertex auth kind is `application_default_credentials` only; accepting an API
  key alias needs a combined env-key-or-ADC auth kind (a build.rs/contract schema
  change) and is left as the named missing primitive.
- NOTE: a later re-run was blocked by an unrelated in-flight `octet-ai` edit
  (`assistant_frame.rs:109` `Media` missing `PartialEq`); my own files compiled
  in the preceding runs.

START 2026-09-15T15:43:45Z ai5 alive

## ai5 (2026-09-15T15:5xZ): 1a.1 Codex `service_tier` — LANDED (unblocks roadmap #175 `/fast`)

- Files: `crates/octet-ai/src/types.rs` (`ServiceTier` enum + `ResponsesRuntimeProfile::accepts_service_tier`),
  `crates/octet-ai/src/responses.rs` (`ResponsesOptions::service_tier` + `with_service_tier`),
  `crates/octet-ai/src/error.rs` (`UnsupportedError::ServiceTier`),
  `crates/octet-ai/src/protocol/openai_responses.rs` (typed body field + fail-closed gate),
  `crates/octet-ai/src/lib.rs` (re-export).
- Gate: the field is emitted only when `request.responses.service_tier` is set AND the
  endpoint's declared `runtime.responses_profile` accepts it
  (`ResponsesRuntimeProfile::accepts_service_tier()`; today only `Codex`, exactly the
  TUI's `/fast` gate). Every other profile returns
  `AiError::Unsupported(UnsupportedError::ServiceTier)` — never a silent drop.
  No provider-name branching; the declaration is the endpoint profile already emitted
  by `crates/octet-coding-agent/build.rs`.
- Wire values: `auto | default | flex | priority` (upstream
  `ResponseCreateParamsStreaming["service_tier"]`, `packages/ai/src/api/openai-responses.ts:321`
  and `openai-codex-responses.ts:566`).
- Command: `cargo test -p octet-ai --lib service_tier` -> observed
  `3 passed; 0 failed` (`service_tier_is_absent_unless_the_caller_requests_it`,
  `codex_service_tier_wire_values_match_the_declared_tiers`,
  `service_tier_fails_closed_on_a_profile_that_does_not_declare_it`).
- Command: `cargo test -p octet-ai --lib` -> observed `342 passed; 0 failed`.
- Activation seam for `/fast`: `request.responses.get_or_insert_with(Default::default).service_tier =
  Some(octet_ai::ServiceTier::Priority)` for `on`, `= None`/omitted for `off`, after checking
  `model.endpoint.runtime.responses_profile.accepts_service_tier()`.
- Bounded gap (recorded, not silently claimed): upstream also runs
  `applyServiceTierPricing` (multiplier 0.5 flex / 2 priority / 2.5 `gpt-5.5` priority,
  `openai-responses.ts:362-389`) over usage cost. octet does not yet scale `Response.cost`
  for a non-standard tier; the named missing primitive is threading the requested/echoed
  tier into `ResponseBuilder::finish` (`crates/octet-ai/src/stream.rs:809-822`), which owns
  the only `cost_of` call on the Responses streaming path.

ai5: service_tier landed

START 2026-09-15T16:28:32Z ai6 alive

START 2026-09-15T16:50:13Z ai7 alive

START 2026-09-15T17:13:25Z ai8 alive

## ai8 (2026-09-15T17:1x-18:xxZ): TASK 1 — Codex websocket drop-protection made real (P0)

### Finding 1 — the row's code was already at HEAD, but `octet-ai` was RED and HANGING
`responses_ws.rs` already contained reconnect/resume/heartbeat code at HEAD
`9c43111d` (the parent's brief described the PRE-wave-6 file: `heartbeat_timeout`
at :65/:775/:798/:930). Reproduced at HEAD with my own runs:
- `cargo test -p octet-ai --lib` -> `FAILED. 4 failed` and
  `responses_ws::tests::reconnect_attempts_and_total_wait_are_bounded` printing
  "has been running for over 60 seconds" and NEVER terminating (killed manually).
- `cargo test -p octet-ai` stopped at the lib target, so the wave-6/7 regression
  in `crates/octet-ai/tests/client_stream.rs` (6 failures) was INVISIBLE to the
  verifier. All 6 exist unchanged at base `df5a7e80`
  (`git diff df5a7e80 HEAD -- crates/octet-ai/tests/client_stream.rs` is empty).

### Base comparison — evidence for "was it regressed?"
Base source `git show df5a7e80:crates/octet-ai/src/responses_ws.rs`:
- `failed_terminals_retire_before_publication_for_text_and_binary`: base ran
  `alive.store(false); disable_key(&state,..).await;` BEFORE
  `command.reply.send(Ok(value))` for every `connection_refresh` event (:850-860),
  which is exactly what the test asserts -> it PASSED at base. This branch
  published the provider failure first (the `Forwarded` outcome) -> **REGRESSED**.
  Fixed by handing the event back unpublished and retiring in `run_connection`
  before sending it (base ordering restored).
- `fatal_events_retire_before_publishing_with_a_contended_pool`: base published
  `heartbeat_timeout(..)` = `Transport{phase: Body, timeout: true}` (or
  `Decode`), so `matches!(error, Transport|Decode)` held, and retirement happened
  before publication -> it PASSED at base. The retire-before-publish property was
  NOT regressed by this branch (the `Fatal` arm still retires first); only the
  error CLASS changed, because this row deliberately replaces the replayable body
  timeout with `StreamProtocolError::ResponseNotResumable`. Expectation updated
  per injected failure (malformed frame -> `Decode`; detected drop after visible
  output -> typed non-resumable with `visible_output: true`).
- `client_stream.rs`: all 6 failures come from deliberate row changes, not from an
  accidental regression: the row added (a) bounded retry of a dropped socket
  before any output, (b) retry of provider-declared connection refreshes/stale
  cursors, (c) the typed non-resumable terminal, (d) buffering of the lifecycle
  prelude so a retry cannot publish an abandoned attempt's prelude twice.

### Code changes (`crates/octet-ai/src/responses_ws.rs`, my path)
1. `AttemptOutcome::Forwarded`/`GenerationEnd::Forwarded` now CARRY the provider
   event instead of publishing it inside the pump; `run_connection` retires the
   socket and fences the pool key BEFORE the event reaches the consumer (base
   invariant `failed_terminals_retire_before_publication_*`).
2. New `flush_pre_output`: this attempt's buffered `response.created`/
   `in_progress` prelude is flushed (i) when a provider failure terminal ends the
   attempt, and (ii) when the bounded budget is exhausted / resumption is
   impossible and the pump returns a typed `Fatal`. A consumer that never gets
   output still sees `Started`/`first_body_seen` (pre-row contract), while a
   RETRY still rebuilds the prelude so nothing is published twice.
3. `pump_generation` takes the prelude buffer from `run_generation` (cleared per
   attempt); decode `Fatal` also flushes it.
4. Hang fix (`reconnect_attempts_and_total_wait_are_bounded`): the fixture
   scripted `MAX+2 = 5` connections but the transport uses exactly
   `MAX+1 = 4` (1 accepted + one replay per attempt), so the scripted server
   blocked forever on a 5th `accept` that never came. Script count corrected and
   every scripted-server join is now bounded (`finish_scripted_server`), so a
   future mismatch is a loud failure, never a hang.
5. `a_drop_before_output_reconnects_and_resumes_with_each_delta_once`: the
   replacement script now emits its own `response.in_progress`; the test asserts
   the abandoned attempt's prelude is NOT duplicated (created==1, in_progress==1)
   and that the replacement's prelude IS delivered. (The old expectation was
   unreachable: attempt 2's script never sent `in_progress`.)
6. `a_mid_stream_drop_resumes_from_the_cursor_with_each_delta_once`: the
   `assert!(connection.alive)` expectation was wrong — after a cursor resume the
   socket that dropped mid-generation is gone, and the actor correctly retires it
   instead of advertising a dead socket as reusable. Replaced with the invariant
   that matters plus a NEW pool-level follow-up: the key is not disabled, the
   dead session is removed, and the next turn on that key dials a fresh session
   and completes.

### Behaviour change (user-visible; must appear in the PR body)
"a heartbeat/transport failure is terminal and must not auto-replay" was REPLACED
by "a heartbeat/transport failure before any consumer-visible output reconnects
within a bounded budget (`MAX_SOCKET_RECONNECT_ATTEMPTS = 3`,
`RECONNECT_TOTAL_BUDGET = 6s`, backoff 250ms->2s) and replays the full local
body; a drop after output is resumed from the `(response_id, sequence_number)`
cursor; the turn is terminal ONLY when the budget is exhausted or resumption is
impossible, and then it fails closed with the typed
`StreamProtocolError::ResponseNotResumable` (never a fabricated success)".
Provider-declared rejections (`websocket_connection_limit_reached`,
`previous_response_not_found`) are retried on a fresh socket, matching upstream
`openai-codex-responses.ts:337-344`; the provider code survives in the bounded
error `detail`.

### Tests updated (client_stream.rs, my path) — each documents old vs new contract
- `responses_websocket_failure_after_send_is_terminal`: now asserts the typed
  non-resumable error with `attempts == 3` and exactly `1 + 3` provider
  generation frames (the bound is observable end to end), no HTTP transport call.
- `responses_websocket_connection_limit_retires_socket_and_falls_back`: keeps
  `Started`/`first_body_seen`, asserts the bounded retry (`1 + 3`), asserts the
  provider code survives in the typed error's `detail`, keeps the HTTP fallback.
- `responses_websocket_failed_output_next_explicit_request_uses_full_http_replay`:
  a post-output drop with no retained response is now the typed non-resumable
  error instead of a replay-safe `TransportPhase::Body` timeout.
- `responses_websocket_heartbeat_timeout_after_created_is_terminal` and
  `..._heartbeat_failure_is_terminal_and_next_request_falls_back`: rewritten for
  the new contract (bounded recovery, `attempts == 3`, heartbeat cause in the
  detail, `1 + 3` frames, `Started` delivered by the final flush, HTTP fallback
  for the caller's next request).
- `responses_websocket_pongs_do_not_extend_response_idle_timeout`: keeps its
  purpose (control Pongs must not extend model progress) and now states plainly
  that the buffered prelude is never delivered when the consumer's own bound
  expires first (`!first_body_seen`, `last_event_ms == None`).
- NEW `responses_websocket_heartbeat_failure_resumes_on_a_fresh_socket` (parent
  request): a genuinely HALF-OPEN first connection (`WebSocketBehavior::Stall`
  now holds the socket open without reading probes instead of closing after
  500ms) followed by a healthy replacement connection -> the turn RESUMES:
  `text == "websocket"`, `Started` exactly once, one bounded reconnect, 2 frames.
- Lib test `an_unrecoverable_mid_stream_drop_yields_the_typed_error_with_a_bounded_retry`
  already pins the terminal-on-budget-exhaustion case (3 bounded resume attempts).

### Store root cause (recorded, NOT changed — cross-boundary)
`client.rs` installs a `ResponseResumer` only when `body_requests_storage(&body)`
is true, i.e. the request asked the provider to retain the response. Every live
Codex request is built by `ResponsesOptions::full_replay(...)`
`store: false` in `crates/octet-agent/src/agent.rs`
(`durable_responses_options:3672-3679`, `native_responses_options:3685-3698`), so
a POST-OUTPUT drop still fails closed with `ResponseNotResumable` instead of
resuming. Pre-output reconnect (the maintainer's long-first-token case) is
unaffected. Missing primitive: those agent-side builders must opt into
`store: true` (upstream Codex's retained session) on a `WebSocketPreferred` Codex
endpoint, or the codec must be told by declaration data. `agent.rs` is another
worker's file. Documented in `docs/parity/codecs.md` §1c.6.

### Observed results (my runs)
- `cargo test -p octet-ai --lib --locked` -> `ok. 351 passed; 0 failed; 0 ignored`
  (was `FAILED. 4 failed` + one hang before this change).
- `cargo test -p octet-ai --lib --locked -- --exact
  responses_ws::tests::reconnect_attempts_and_total_wait_are_bounded --nocapture`
  -> `ok. 1 passed; ... finished in 1.79s` (previously >50 min, never terminated).
- `cargo test -p octet-ai --locked` (whole package, 13 targets) -> `EXIT=0`:
  lib 351, bedrock_current 5, client_batch 3, client_compact 18, client_complete
  1, client_stream 38, coverage_manifest 3, google_current 2, mistral_current 16,
  provider_discovery 4, public_api 1, smoke 1, doc-tests 1 — every target
  `0 failed`, no hang.
- `rustfmt --edition 2021 --check crates/octet-ai/src/responses_ws.rs`: my added
  code is clean; the 22 remaining hunks are pre-existing (wave-6/7 code and older
  tests) and were left alone (no workspace-wide rustfmt, per the brief).

### Doc correction (parent/V2 request)
`docs/parity/providers.md` no longer headlines Codex `service_tier` as unblocking
`/fast`: the section is now "field landed; `/fast` NOT yet unblocked", states that
no live run selects a tier, and names the exact missing primitive (the agent-side
`ResponsesOptions` builders) as Gap 1, with the pricing multiplier as Gap 2.

### CHANGELOG-ready bullet
"Codex Responses websockets no longer kill a turn on a drop: a bounded-backoff
reconnect (3 attempts, 6s ceiling, 250ms->2s backoff) replays the full local body
when nothing visible was published, a mid-stream drop is resumed from the
`(response_id, sequence_number)` cursor so every delta arrives exactly once, and
the turn fails closed with a typed `ResponseNotResumable` error when resumption is
impossible or the budget is spent — a provider failure still retires and fences
the pooled session before the consumer can observe it."

ai8: TASK 1 landed; octet-ai fully green (13 targets) with no hang.

START 2026-09-15T17:53:03Z ai9 alive

START 2026-09-15T17:55:34Z ai11 alive

ai11 TASK 1 (#388 Responses computer_call lifecycle) — LANDED (wire protocol only)
- `crates/octet-ai/src/responses.rs`: public `ComputerUseTool` + `ComputerUseEnvironment` (typed wire declaration) and `ResponsesOptions.computer_use` (`#[serde(default)]`, `skip_serializing_if`) + `ResponsesOptions::with_computer_use`.
- `crates/octet-ai/src/types.rs`: declarative endpoint gate `ResponsesRuntimeProfile::accepts_computer_use()` (Default profile only; no provider-name branch). `crates/octet-ai/src/error.rs`: `UnsupportedError::ComputerUse` fail-closed error.
- `crates/octet-ai/src/protocol/openai_responses.rs`: declaration (`{"type":"computer_use_preview","display_width","display_height","environment"}`), decode of `computer_call` -> canonical tool call named `computer_use_preview` with bounded arguments `{"action":…,"pending_safety_checks":…}` (allowlist click/double_click/drag/keypress/move/screenshot/scroll/type/wait; 16 KiB action bound), and dispatch of the canonical tool result back to `computer_call_output` with the single documented `computer_screenshot` object (4 MiB inline screenshot bound) for both canonical and opaque-replay paths.
- Fail-closed: unknown/absent action, over-bound action, actionless terminal computer call, non-declaring profile, Responses Lite.
- Fixture `crates/octet-ai/tests/fixtures/openai_responses/computer_call.sse`.
- No authority added: no desktop/browser backend, no host action path. Remaining primitive for the authority half is #383 (host-gated scoped authorization: `crates/octet-ai/src/protocol/openai_responses.rs` will carry `computer_call_output` but nothing in octet may execute the action; the missing host primitive is an approved-action executor + policy decision in `crates/octet-coding-agent/src/host/policy.rs`, owned elsewhere).

Observed output (`cargo test -p octet-ai --lib computer`):
```
running 13 tests
test protocol::openai_responses::fixture_tests::computer_call_round_trips_action_call_id_and_safety_checks ... ok
test protocol::openai_responses::fixture_tests::computer_call_decodes_identically_across_byte_boundaries ... ok
test protocol::openai_responses::fixture_tests::unsupported_computer_action_fails_closed ... ok
test protocol::openai_responses::fixture_tests::computer_call_without_an_action_fails_closed ... ok
test protocol::openai_responses::fixture_tests::oversized_computer_action_fails_closed ... ok
test protocol::openai_responses::fixture_tests::terminal_computer_call_action_is_used_when_added_omits_it ... ok
test protocol::openai_responses::tests::computer_use_declaration_matches_the_documented_wire_tool ... ok
test protocol::openai_responses::tests::computer_use_fails_closed_on_a_profile_that_does_not_declare_it ... ok
test protocol::openai_responses::tests::computer_use_is_absent_unless_the_caller_declares_it ... ok
test protocol::openai_responses::tests::computer_call_history_replays_as_computer_call_and_output ... ok
test protocol::openai_responses::tests::computer_call_output_stays_bounded_when_no_screenshot_is_available ... ok
test protocol::openai_responses::tests::oversized_inline_screenshot_is_not_forwarded ... ok
test protocol::openai_responses::tests::undocumented_canonical_computer_action_is_not_replayed ... ok
test protocol::openai_responses::tests::opaque_replay_dispatches_computer_results_by_authoritative_output ... ok
test result: ok. 13 passed; 0 failed
```
(`opaque_replay_dispatches_computer_results_by_authoritative_output` observed item pair:
`[{"action":{"type":"screenshot"},"call_id":"call_comp_1","id":"cc_1","status":"completed","type":"computer_call"},{"call_id":"call_comp_1","output":{"type":"computer_screenshot"},"type":"computer_call_output"}]`)

Observed `cargo test -p octet-ai --lib`: `test result: ok. 366 passed; 0 failed; 0 ignored; 0 measured; 366 filtered out? -> 353 filtered out` (no hang; `responses_ws::tests::reconnect_attempts_and_total_wait_are_bounded` ok).

ai11 TASK 2 (verifier C2, Codex `service_tier` headline) — ALREADY TRUE IN TREE; no false headline found
- `docs/parity/providers.md:40` at HEAD `00e3ca3e` reads `## Codex \`service_tier\` (row 1a.1 — field landed; \`/fast\` NOT yet unblocked)`; the body (`:42-48`) and Gap 1 (`:69-75`) already state that the live-run `ResponsesOptions` builders in `crates/octet-agent/src/agent.rs` never set a tier and name the missing primitive (`with_service_tier` at `durable_responses_options` / `native_responses_options`).
- Evidence the C2 finding is stale: `git diff --stat -- docs/parity/providers.md` is empty (unmodified from HEAD) and `rg -n service_tier crates/octet-agent/src/agent.rs` returns nothing, so the codec field is still inert and the file already says so. `docs/parity/VERIFICATION.md:471` (verifier-owned, not edited here) still quotes the old "landed, unblocks roadmap #175 `/fast`" headline.
- Added: a status note under the headline recording the re-check, plus the watch condition (`agent10: service_tier plumbed into the live run path` — searched `docs/swarm-audit/EXECUTION-agent3.md`: absent as of now). Gap 1 already names the primitive; no further edit needed.

ai11 TASK 3 rows — state verified in-tree (no code claimed for rows not landed)
- 1a.2 PiMessages: NOT landed (`rg -l PiMessages crates/` empty). Needs `Protocol::PiMessages` + codec + client dispatch + catalog registration.
- 1c.5 Bedrock profiles / 1c.6 per-request transport + connect deadline + debug stats / 1c.7 Azure deployment map / 1c.9 xAI encrypted-reasoning replay: NOT landed (`rg -l "profile_arn|application-inference-profile|websocket-cached|connect_deadline|responseModel|providerThinkingLevel|rawStopReason" crates/` empty; `deployment` only in generated provider fixtures/contract).
- 1b.3 proxy/NO_PROXY: LANDED before this turn in `crates/octet-ai/src/declarations/proxy.rs` with root/subdomain exclusion (`example.com` matches `api.example.com`, not `notexample.com`) and a test table; not re-claimed here.
- 1b.4 conditional inventory etag: not in `octet-ai`; owning test/impl is `crates/octet-coding-agent/src/providers/conditional_inventory_tests.rs` + `app/bootstrap.rs` (bootstrap.rs is NOT this worker's path).
- 1b.6 `GOOGLE_CLOUD_API_KEY`: BLOCKED BY PATH OWNERSHIP, not policy. The vertex declaration is `{"kind":"application_default_credentials"}` (`crates/octet-coding-agent/src/providers/declarations.json:636`); a combined env-key-or-ADC kind must be added to the contract generator at `crates/octet-coding-agent/build.rs` (`AuthenticationSpec`, `:125`, `:717`, `:866`, `:1040`) and `crates/octet-coding-agent/src/providers/contract.rs` is generated output — `build.rs` is outside this worker's exclusive paths, and hand-editing a generated artifact is forbidden. Exact primitive: `AuthenticationSpec::ApiKeyOrApplicationDefaultCredentials { env: &[&str] }` in `build.rs` + regenerate `contract.rs`, then fail closed when neither the env key nor ADC is present.
- 1d.1/1d.2/1d.3 auth depth: not landed in `octet-ai/src/auth.rs` (typed plumbing for `apiKey.check/resolve`, `oauth.login/refresh/logout`, `AuthCheck`, `minOAuthValidityMs`, `isSubscription`, unified credential store) — no host-brokered OAuth/credential POLICY changes were made or proposed.
- 1c.10 response metadata: BLOCKED BY PATH OWNERSHIP. `Response` literals are constructed outside my paths (`crates/octet-agent/src/context.rs:467`, `crates/octet-ai/src/protocol/openai_chat.rs:1373`), and `ToolResult` literals exist in `crates/octet-agent/**` and `crates/octet-coding-agent/**`; adding public fields breaks those constructors regardless of `#[serde(default)]`. Exact primitive: add `ResponseMetadata { response_model, provider_thinking_level, raw_stop_reason, diagnostics }` + `ToolResult.usage` behind `#[serde(default)]` in `crates/octet-ai/src/types.rs` in the same change as the downstream literals (needs a coordinated multi-owner edit).
- 1c.3 remainder (OAuth/fine-grained/interleaved/mid-conversation betas): BLOCKED BY PATH OWNERSHIP. It needs a `compat` record on `ModelSpec` (`crates/octet-ai/src/types.rs`), but `ModelSpec { … }` literals exist in `crates/octet-agent/**` and `crates/octet-coding-agent/**` (e.g. `crates/octet-coding-agent/src/app/bootstrap.rs`, `providers/catalog.rs`, `tui/**`), so the one-field change cannot land without breaking constructors this worker does not own. Caller-beta merge is already landed (`caller_anthropic_beta_list_is_authoritative_and_deduplicated`).
- 1c.4 fallback-model pricing: not landed; coupled to the same `Response` metadata addition above (a tier/fallback-aware `cost_of` needs the echoed response model).
- 1e.1 faux provider deferred handles / 1e.3 image generation: not started (no budget left this turn).

CHANGELOG-ready bullet (ai11, TASK 1 / roadmap row #388):
- **OpenAI Responses computer use (wire protocol).** `octet-ai` now declares the
  provider `computer_use_preview` tool when a caller selects it
  (`ResponsesOptions::with_computer_use`) and the route's declared
  `ResponsesRuntimeProfile` accepts it, maps a provider `computer_call` to a
  canonical tool call named `computer_use_preview` with a bounded action payload
  (`{"action":…,"pending_safety_checks":…}`, allowlisted action types, 16 KiB
  cap), and dispatches the caller's result back as a `computer_call_output` item
  carrying the single documented `computer_screenshot` (4 MiB inline cap).
  Unknown, absent, or over-bound actions fail closed. No desktop or browser
  backend is included: nothing in octet executes a computer action, and whether
  any action may run remains a host-policy decision (roadmap #383).

START 2026-09-15T18:10:03Z ai11 alive (startup latency P0)

START 2026-09-15T18:21:34Z ai12 alive

START 2026-09-15T18:29:32Z ai12b alive

START 2026-09-15T19:02:45Z ai12c alive

## ai12c — TASK 1 (P0 AWS metadata activation), TASK 2/3 verification, TASK 4 rows

### TASK 1 — adopted wave-11 work, fixed it, proved it (observed output)

The wave-11 rule was in-flight and **broken**: `cargo test -p octet-coding-agent
--lib -- providers::auth` failed 2/29 before my edits. Fixes:
- `metadata_endpoint_precedence_is_standard_name_then_alias_then_profile`: the
  accepted endpoint is normalized with a trailing slash (needed so IMDS path
  segments append); the test expected un-normalized strings. Test corrected.
- `opt_in_activation_resolves_and_signs_with_live_metadata_credentials`: resolved
  the blocking metadata client inside an async test (`Cannot drop a runtime in a
  context where blocking is not allowed`). Now resolves in `spawn_blocking`,
  exactly like `AwsBedrockSigner::sign`.
- `disabled_activation_opens_no_connection_to_a_live_metadata_endpoint`: the
  live-socket control connection raced a non-blocking `accept` against the TCP
  handshake and was flaky under parallel load. Rewritten with a blocking accept,
  an explicit drain barrier, and a bounded wait for the fixture's observation;
  the behavior assertion is still a request count. Ran 5× consecutively: 5/5 ok.

Observed (`cargo test -p octet-coding-agent --lib -- providers::auth`):
`test result: ok. 40 passed; 0 failed; 0 ignored; 0 measured; 1402 filtered out`.
The zero-request rule is asserted by counters
(`unrelated_provider_launch_makes_zero_aws_metadata_requests`,
`disabled_activation_opens_no_connection_to_a_live_metadata_endpoint`, plus the
pure-function table across `AWS_EC2_METADATA_DISABLED` true/false/absent,
profile present/absent, static keys present/absent, and the explicit opt-in), and
the intentional path by
`opt_in_activation_resolves_and_signs_with_live_metadata_credentials`,
`an_ec2_instance_identity_still_resolves_instance_credentials`,
`indicated_metadata_probe_reaches_the_ec2_source`,
`the_profile_endpoint_activates_and_is_the_probe_target`. No timing threshold is
used anywhere. Documented in `docs/providers.md` (indication order, fail-closed
unknown state, measured 1,025/1,027 ms → 22/19 ms, test names).

CHANGELOG-ready: **AWS metadata credentials no longer cost unrelated-provider
launches ~1 s.** A pure activation rule (`aws_metadata_activation_from`) keeps
EC2/ECS metadata probes closed unless the local environment positively indicates
them (standard disable switch off, `OCTET_AWS_METADATA_CREDENTIALS`, container
URIs, pinned IMDS endpoint, a profile that declares metadata, or local DMI
markers naming `Amazon EC2`), fails closed on unknown state, and is asserted by
request counts; indicated EC2/ECS-backed Bedrock runs still resolve and sign.

### TASK 2 — Responses `computer_call` lifecycle: already landed in wave 10, verified

`rg -n computer_call crates/octet-ai/src/responses.rs` is no longer empty: the
declaration (`ComputerUseTool`, `ResponsesOptions::with_computer_use`), the
`computer_call` → canonical `computer_use_preview` mapping, and the
`computer_call_output` dispatch (single `computer_screenshot`, 4 MiB inline cap,
16 KiB action cap, unknown/absent/oversized actions fail closed) are in
`crates/octet-ai/src/protocol/openai_responses.rs`, with the scripted SSE fixture
`crates/octet-ai/tests/fixtures/openai_responses/computer_call.sse`. Observed:
`cargo test -p octet-ai --lib -- computer` → `13 passed; 0 failed`, including
`computer_call_round_trips_action_call_id_and_safety_checks`,
`computer_call_decodes_identically_across_byte_boundaries`,
`unsupported_computer_action_fails_closed`,
`computer_call_history_replays_as_computer_call_and_output`. Wire protocol only:
no desktop/browser backend, no authority to act (#383 host-gated). No code change
was needed or made by this worker.

### TASK 3 — Codex `service_tier` doc truthfulness

Re-read the builders (`crates/octet-agent/src/agent.rs`): `resolve_service_tier`
(`:3786`) rejects any tier unless `Protocol::OpenAiResponses` **and**
`responses_profile.accepts_service_tier()` (Codex only, `types.rs:218`); both
`durable_responses_options` (`:3750`) and `native_responses_options` (`:3801`)
route the validated tier through `ResponsesOptions::with_service_tier`
(`responses.rs:362`); `responses_prewarm_request` (`:5780`) reuses them; the codec
re-checks the declaration independently
(`protocol/openai_responses.rs:1154`). `docs/parity/providers.md` updated: stale
line refs (`set_service_tier` `:6505`, accessor `:6512`, `apply_fast_command`
`:1651`), the codec-side re-check added, and the two behavioral tests named.
Residual gap unchanged and now precise: `apply_fast_command`
(`modes/interactive.rs:1651`) still never calls `Agent::set_service_tier`
(`rg -n set_service_tier crates/` finds only the definition plus the agent's
tests), so no live run selects a tier: "agent API ready, UI consumer pending".

### TASK 4 — rows

**1a.2 PiMessages — BLOCKED BY OWNERSHIP (exact primitive).** Adding
`Protocol::PiMessages` breaks four exhaustive matches with no wildcard arm:
`crates/octet-agent/src/telemetry.rs:943`,
`crates/octet-agent/src/extension_process.rs:9593`,
`crates/octet-coding-agent/src/batch.rs:220`,
`crates/octet-coding-agent/src/modes/rpc.rs:461` (find them with
`rg -n Protocol::MistralConversations`). Three of the four are in paths this
worker must not edit (`crates/octet-agent/**`, `modes/**`); a declarative route
also needs `"pi_messages"` in `crates/octet-coding-agent/build.rs:320` plus a
regenerated `contract.rs`. Recorded in `docs/parity/codecs.md`.

**1c.5 Bedrock — API key + web identity landed (this worker).** In
`crates/octet-coding-agent/src/providers/auth.rs`:
`AWS_BEARER_TOKEN_BEDROCK` → `Auth::BearerEnv` (before SigV4, blank value falls
back); `AWS_ROLE_ARN` + `AWS_WEB_IDENTITY_TOKEN_FILE` (+ optional
`AWS_ROLE_SESSION_NAME`) → one bounded STS `AssumeRoleWithWebIdentity` exchange
(3 s, 64 KiB token cap, no retries, `AWS_ENDPOINT_URL_STS` override validated
fail-closed, XML parse requires all three credential fields, errors surface the
STS code and never provider prose or the token); chain order is now env keys →
web identity → profile → indicated metadata sources. Observed:
`cargo test -p octet-coding-agent --lib -- providers::auth` → `40 passed; 0
failed`, including `web_identity_posts_the_documented_sts_form_and_resolves_credentials`
(loopback STS fixture; the resolved credentials sign a SigV4 request) and
`web_identity_sts_failure_is_reported_and_never_downgraded`.
Residual: profile-ARN/application-inference-profile region derivation needs the
model id plumbed from `app/bootstrap.rs`; a declarative bearer kind needs
`AuthenticationSpec` in `build.rs`.

CHANGELOG-ready: **Bedrock now accepts `AWS_BEARER_TOKEN_BEDROCK` (API key) and
web-identity roles** (`AWS_ROLE_ARN`/`AWS_WEB_IDENTITY_TOKEN_FILE`, one bounded
STS exchange, `AWS_ENDPOINT_URL_STS` override); a half-configured web identity
fails closed instead of resolving a different identity.

**1c.9 xAI Responses — plumbing verified, route flip deliberately not taken.**
`include: ["reasoning.encrypted_content"]` (`openai_responses.rs:1129`), the
terminal backfill (`:1528`, tests `:3522`/`:3550`), and canonical replay of the
opaque reasoning item (`:873`) are already in-tree and green
(`cargo test -p octet-ai`). The remaining primitive is the xAI route itself:
upstream pi declares the whole xAI provider as `openai-responses` while octet
declares `openai_chat`; flipping it changes the live wire for every xAI user and
needs a live acceptance run, so it is recorded rather than flipped silently.

**Verified already-landed rows (no code claimed):**
`cargo test -p octet-ai --lib -- proxy` → `5 passed` (1b.3 root/subdomain
exclusion incl. `notexample.com`, port/`*` entries);
`cargo test -p octet-coding-agent --lib -- conditional_inventory` → `5 passed`
(1b.4 200→304 keeps the last good body and advances `checked_at`; 200 without an
etag clears the validator; scoped validators; errors preserve the last good
body).

**Not landed, blocked (unchanged):** 1b.6 `GOOGLE_CLOUD_API_KEY`
(`AuthenticationSpec` in `build.rs`); 1c.3/1c.10 add public `ModelSpec`/`Response`
fields whose literal constructors live in `octet-agent`/`octet-coding-agent`
(constructors this worker does not own); 1c.4 depends on 1c.10; 1c.6 per-request
transport / 1c.7 per-call Azure overrides need a `Request` field (20+ literal
constructors outside this worker's paths); 1e.1 deferred handles need
`AssistantMessage.deferred` (same constructor problem); 1e.3 image generation was
not started (budget); 1d.1-1d.3 not attempted.

