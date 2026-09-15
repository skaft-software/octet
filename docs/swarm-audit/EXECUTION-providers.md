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
