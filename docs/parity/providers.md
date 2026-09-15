# Providers parity detail (rows 1a.1–1d.3)

Reference (read-only): `earendil-works/pi`
@ `8a7b0c03dfb702663acafb6dc29f8acaa4ffe391`. This document owns the exact
upstream anchors and the per-subitem outcome for the provider-owned rows in
[`README.md`](README.md). It is an outcome ledger, not a compatibility claim.
Octet's provider presets are **declarative data** (`src/providers/declarations.json`
→ generated `contract.rs`); no provider identity branches the agent loop, and no
upstream TypeScript is vendored.

Anchors below are `path:line` inside the reference checkout unless prefixed with
`octet:`.

## 1a.1 — baseten / qwen-token-plan[\-cn, -individual] / zai-coding-cn

Upstream anchors: `packages/ai/src/providers/baseten.ts:1`,
`qwen-token-plan.ts:1`, `qwen-token-plan-cn.ts:1`,
`qwen-token-plan-individual.ts:1`, `zai-coding-cn.ts:1`,
`env-api-keys.ts:66` (env map), `scripts/generate-models.ts:1265`
(`processZaiModels`), `:1325` (`processBasetenModels`), `:2388`
(`alibaba-token-plan` → `qwen-token-plan*`), `test/baseten-models.test.ts:1`.

Outcome (landed, declared): octet:
`crates/octet-coding-agent/src/providers/declarations.json` gains `baseten`,
`qwen-token-plan`, `qwen-token-plan-cn`, `qwen-token-plan-individual`,
`zai-coding-cn`: OpenAI Chat route, bearer presentation, `openai_models`
discovery, `inventory_cache: required`, `pricing: reference`. Behavioral test:
octet:`crates/octet-coding-agent/src/providers/contract.rs`
`token_plan_and_coding_provider_declarations_are_declared`.

Gap: upstream ships generated static model catalogs
(`providers/data/<id>.json`, produced by `scripts/generate-models.ts` against
`https://models.dev/api.json`). Those generated files are absent from the
read-only reference checkout and octet has no vendored copy, so the exact static
model lists (pinned ids, `chatTemplateArgs`, reasoning maps) are not landed. The
route/discovery/credential declaration is landed; the static catalog needs a
models.dev generation pass. Deterministic checks here do not qualify live
provider availability.

## Codex `service_tier` (row 1a.1 — landed, unblocks roadmap #175 `/fast`)

Upstream anchors: `packages/ai/src/api/openai-responses.ts:105`, `:321`
(`params.service_tier = options.serviceTier`), `:362-389`
(`getServiceTierCostMultiplier` / `applyServiceTierPricing`);
`packages/ai/src/api/openai-codex-responses.ts:75`, `:566`, `:600-625`
(same field and multipliers on the Codex envelope).

Outcome (landed): octet:`crates/octet-ai/src/types.rs` adds the typed
`ServiceTier` (`auto | default | flex | priority`) and
`ResponsesRuntimeProfile::accepts_service_tier`, the declared capability.
octet:`crates/octet-ai/src/responses.rs` (`ResponsesOptions::service_tier`,
`with_service_tier`) carries the per-request selection;
octet:`crates/octet-ai/src/protocol/openai_responses.rs` emits the
`service_tier` body field and fails closed with
`UnsupportedError::ServiceTier` for any profile that does not declare it.
Behavioral tests: `service_tier_is_absent_unless_the_caller_requests_it`,
`codex_service_tier_wire_values_match_the_declared_tiers`,
`service_tier_fails_closed_on_a_profile_that_does_not_declare_it`
(`cargo test -p octet-ai --lib service_tier` -> 3 passed).

Gap: `applyServiceTierPricing` (usage-cost multiplier 0.5 flex, 2 priority,
2.5 for `gpt-5.5` priority) is not applied to `Response.cost`. Named missing
primitive: thread the requested/echoed tier into
`ResponseBuilder::finish` (`crates/octet-ai/src/stream.rs:809-822`, the only
`cost_of` call on the Responses streaming path). Until then octet reports the
catalog-rate cost and never a fabricated tier-adjusted one.

## 1b.1 — per-request overrides

Upstream anchors: `packages/ai/src/types.ts:130` (`fetch`),
`:140` (`env`), `:145` (`onPayload`), `:149` (`onResponse`),
`:158` (`headers`), `:163` (`timeoutMs`), `:168` (`maxRetries`),
`:176` (`maxRetryDelayMs`); `packages/ai/src/models.ts:646` (provider option
merge), `:662` (`transformHeaders`), `:665` (strip `transformHeaders` before
adapter call).

Outcome (partial, declared plumbing): octet:
`crates/octet-ai/src/declarations/mod.rs` `RequestOverrides` captures the
data-only knobs (`headers`, `env`, `timeout_ms`, `max_retries`,
`max_retry_delay_ms`) with fail-closed bounds and a passing unit test.

Gap (boundary): `apiKey`, `fetch`/transport hook, `onPayload`, `onResponse`,
`transformHeaders` and `metadata` are host-owned *runtime* hooks and are not
data. Octet already expresses the transport hook through
octet:`crates/octet-ai/src/host_transport.rs` (`HostStreamTransport`) and the
authentication lifecycle; the remaining primitive is a per-request
transformer/payload interception seam in octet:`crates/octet-ai/src/client.rs`,
which is in the codec-depth-owned portion of `octet-ai` and is therefore
**reported, not changed** (see the `AI_PATHS_RELEASED` note in the execution
receipt).

## 1b.2 — model/request samplingParams and per-model headers

Upstream anchors: `packages/ai/src/types.ts:861` (`Model.samplingParams`),
`:862` → `StreamOptions.samplingParams` at `:193` (merged over model values,
per-request wins), `:863` (`Model.headers`); applied by
`packages/ai/src/api/openai-completions.ts` and
`packages/ai/src/api/openai-responses.ts:355`.

Outcome (declared plumbing): octet `ModelPreset::sampling_params` and
`ModelPreset::headers` in octet:`crates/octet-ai/src/declarations/mod.rs`, with
validation (non-empty keys, header token/value checks, size bound) and
round-trip + rejection tests.

Gap: the OpenAI-compatible codecs that would merge these into the request body
are codec-depth-owned (octet:`crates/octet-ai/src/protocol/*`,
octet:`crates/octet-ai/src/client.rs`); merge wiring is reported, not changed.

## 1b.3 — HTTP_PROXY / HTTPS_PROXY / ALL_PROXY / NO_PROXY

Upstream anchors: `packages/ai/src/utils/node-http-proxy.ts:74`
(`shouldProxyHostname`), `:85` (comma/space list), `:91` (optional port),
`:96` (`*.` / `.` / `*` prefixes), `:106` (exact host) and `:110`
(`.${domain}` suffix → root **and** subdomain excluded), `:79` (`*` disables
proxying), `:131` (`${protocol}_proxy` then `all_proxy`, scheme defaulting),
`:141` (`resolveHttpProxyUrlForTarget`, http/https only).

Outcome (core landed, wiring blocked-by-boundary): octet:
`crates/octet-ai/src/declarations/proxy.rs` reproduces the upstream resolver as
pure logic — `resolve_http_proxy` (scheme proxy then `all_proxy`, scheme
defaulting, http/https only), `no_proxy_excludes` (root **and** subdomain via
exact, `.domain` and `*.domain`; optional `:port`; lone `*` disables; `*` inside
a list is a no-op) and `proxy_env_value` (lower-case before upper-case). Unit
tests: `no_proxy_root_and_subdomain_are_both_excluded`,
`no_proxy_port_and_star_entries`, `proxy_scheme_defaults_and_all_proxy_fallback`,
`unsupported_and_malformed_proxies_fail_closed`, `env_lookup_prefers_lowercase`.
Octet builds its streaming client at octet:`crates/octet-ai/src/client.rs:1891`
(`reqwest::Client::builder()`), which is in the codec-depth-owned `octet-ai`
portion.

Missing primitive: a `client.rs` `reqwest::Proxy`/`NoProxy` seam that calls this
resolver. reqwest already honors the proxy environment variables by default, but
matching upstream's exact root+subdomain `NO_PROXY` semantics and the
http/https-only rejection requires that seam. It lives across the released
boundary and is reported rather than changed.

## 1b.4 — conditional catalog etag / If-None-Match / checkedAt

Upstream anchors: `packages/ai/src/models-store.ts:3` (`ModelsStoreEntry`),
`:5` (`lastModified`), `:8` (`checkedAt`), `:13` (opaque `etag` echoed as
`If-None-Match`).

Outcome (landed): octet:`crates/octet-coding-agent/src/app/bootstrap.rs`
`ProviderInventoryCache { etag, checked_at }`, `inventory_etag` (bounded,
ASCII, valid header value), `refresh_provider_inventory_with` (sends
`If-None-Match` only from a provider/URL/credential-scoped cache, handles 200
and 304, clears a stale validator, preserves last-good on failure). Behavioral
test: octet:`crates/octet-coding-agent/src/providers/conditional_inventory_tests.rs`
(five tests incl. a real loopback 200→304 exchange), wired via
octet:`crates/octet-coding-agent/src/app/bootstrap.rs` `#[path = ...]`.

Gap: upstream also tracks `lastModified` (Last-Modified); octet stores only
`etag` + `checked_at`. The upstream cache-version bump is not needed since old
records deserialize with `None` (asserted by the legacy-record test).

## 1b.5 — vllmPriority, supportsMaxOutputTokens, thinkingTokenBudgetField, chatTemplateArgs/Kwargs, $var interpolation, string thinking

Upstream anchors: `packages/ai/src/types.ts:642` (`vllmPriority`),
`:664` (`supportsMaxOutputTokens`), `:619`/`:621`
(`thinkingTokenBudgetField` / `supportsThinkingTokenBudget`), `:603`/`:605`
(`chatTemplateKwargs` / `chatTemplateArgs`), `:590` (`thinkingFormat`, incl.
`"string-thinking"`), `:602` (`{ "$var": ... }`);
`packages/ai/src/api/openai-completions.ts` `resolveChatTemplateKwargValue`
(`thinking.enabled` → bool, `thinking.budget` → number, `thinking.effort` →
level-mapped, `omitWhenOff` → drop).

Outcome (declared plumbing): octet `ModelPreset` (`vllm_priority`,
`supports_max_output_tokens`, `thinking_token_budget_field`,
`chat_template_args`/`chat_template_kwargs`, `thinking_format`),
`ThinkingFormat::StringThinking` (`"string-thinking"`),
`ThinkingTokenBudgetField::{Vllm,Qwen,LlamaCpp}` field names, and
`ChatTemplateValue`/`ThinkingSelection::interpolate_chat_template` reproducing
the upstream `$var` semantics, all in octet:`crates/octet-ai/src/declarations/mod.rs`
with passing unit tests (`chat_template_arguments_interpolate_variables_and_literals`,
`chat_template_omits_unmapped_and_off_values`,
`unknown_chat_template_variable_fails_closed`,
`string_thinking_is_distinct_and_excludes_chat_template_args`).

Gap: the OpenAI-Chat codec that emits `chat_template_args`/`priority`/
`max_output_tokens`/`thinking_token_budget` is codec-depth-owned; emission wiring
is reported, not changed. `supportsThinkingTokenBudget` (the boolean alias) is
modelled as `thinking_token_budget_field: "thinking_token_budget"`.

## 1b.6 — ANTHROPIC_AUTH_TOKEN, ANTHROPIC_OAUTH_TOKEN, GOOGLE_CLOUD_API_KEY aliases

Upstream anchors: `packages/ai/src/env-api-keys.ts:23`
(`ANTHROPIC_AUTH_TOKEN_ENV`), `:24` (`ANTHROPIC_OAUTH_TOKEN_ENV`), `:69`
(anthropic env list order `[AUTH_TOKEN, OAUTH_TOKEN, API_KEY]`), `:102`
(`getEnvApiKey` skips `ANTHROPIC_AUTH_TOKEN` because it must be sent as
`Authorization: Bearer`), `:77` (`GOOGLE_CLOUD_API_KEY` for `google-vertex`).

Outcome (landed for Anthropic): octet:
`crates/octet-coding-agent/src/providers/auth.rs` `bearer_token_variable` maps
`ANTHROPIC_AUTH_TOKEN` / `ANTHROPIC_OAUTH_TOKEN` to `Authorization: Bearer`
(the route's static `auth_presentation` is otherwise `x-api-key`), keyed on the
credential variable rather than a provider name. octet:
`crates/octet-coding-agent/src/providers/declarations.json` lists
`[ANTHROPIC_AUTH_TOKEN, ANTHROPIC_OAUTH_TOKEN, ANTHROPIC_API_KEY]`. Tests:
`bearer_token_aliases_override_the_route_api_key_header`,
`anthropic_declaration_lists_bearer_aliases_before_api_key`.

Gap: `GOOGLE_CLOUD_API_KEY` for `google-vertex` is not landed. octet's vertex
declaration is `application_default_credentials` only
(`declarations.json`), so accepting an API-key alias needs a combined
env-key-or-ADC auth kind — a `build.rs`/generated-contract schema change. This is
the named missing primitive; declared plumbing (`ProviderCredentialPreset`) in
octet:`crates/octet-ai/src/declarations/mod.rs` already records the alias shape.

## 1d.1 — apiKey.check/resolve, oauth.login/refresh/logout, AuthCheck, minOAuthValidityMs, isSubscription

Upstream anchors: `packages/ai/src/auth/types.ts:112` (`AuthCheck`), `:186`
(`check`), `:194` (`resolve`), `:211` (`isSubscription`);
`packages/ai/src/auth/resolve.ts:22` (`minOAuthValidityMs`), `:135`
(`max(DEFAULT, override)`), `:169` (refresh when expiring soon), `:189`
(`apiKey.resolve`).

Outcome: **blocked-by-policy** for the OAuth/refresh/login/logout surface
(broker policy is excluded per README "Non-negotiable exclusions"). The
declarative `AuthCheck`/`isSubscription`/`minOAuthValidityMs` shape is not yet
added: octet's octet:`crates/octet-agent/src/extension_provider.rs` already
owns the host authorization-policy trait, and duplicating a second auth-policy
surface in `octet-ai` would risk bypassing it. Missing primitive: an explicit
host hook that surfaces `check`/`resolve`/`minOAuthValidityMs` from the existing
`octet-agent` provider trait into `octet-ai` without a second policy.

## 1d.2 — unified tagged provider credential store, list/modify/delete, provider-scoped env

Upstream anchors: `packages/ai/src/auth/credential-store.ts:1`,
`packages/ai/src/auth/helpers.ts:1`.

Outcome: **blocked-by-policy**. octet already keeps provider-scoped secrets
behind its private credential lifecycle
(octet:`crates/octet-coding-agent/src/providers/auth.rs:1`) and a host-brokered
store. A parallel "unified tagged store" with its own list/modify/delete would
duplicate policy. Missing primitive: an explicit decision on whether the
existing store gains tag/list/modify/delete or a new store is authorized; this
is a host-policy question, not a code gap this worker may resolve.

## 1d.3 — Anthropic OAuth, xAI device, OpenRouter PKCE/manual redirect, Kimi device; RFC8628/PKCE/callback helpers

Upstream anchors: `packages/ai/src/auth/oauth/anthropic.ts:1`,
`xai.ts:1`, `openrouter.ts:1`, `kimi-coding.ts:1`, `pkce.ts:1`,
`device-code.ts:1`.

Outcome: **blocked-by-policy**. These are OAuth flows whose depth must preserve
the existing host policy rather than bypass it (README "Non-negotiable
exclusions"; "OAuth flow depth must preserve the existing host policy").
No flow was added. Missing primitive: a host-brokered OAuth entry point (device
and PKCE) that the existing `octet-agent` policy drives; adding RFC8628/PKCE
helpers in `octet-ai` without that host seam would either be dead code or a
policy bypass.

## Rows landed as code + test in this pass

| Row | Code | Test (command) |
| --- | --- | --- |
| 1a.1 | declarations.json + contract test | `cargo test -p octet-coding-agent --lib token_plan_and_coding_provider_declarations_are_declared` |
| 1b.2/1b.5 (declared) | octet-ai `declarations` module | `cargo test -p octet-ai --lib declarations::tests` |
| 1b.3 (core) | octet-ai `declarations/proxy.rs` | `cargo test -p octet-ai --lib declarations::proxy` |
| 1b.6 | auth.rs + declarations.json | `cargo test -p octet-coding-agent --lib providers::auth` |
| 1b.4 | bootstrap.rs conditional inventory | `cargo test -p octet-coding-agent --lib provider_conditional_inventory_tests` |

Rows reported across the released `octet-ai` boundary (1b.1 runtime hooks,
1b.3 proxy resolver) and blocked-by-policy rows (1d.1–1d.3) are named above with
their exact missing primitive.
