# Providers and models

[Documentation](README.md) · [Configuration](configuration.md) · [Media](media.md)

```sh
export ANTHROPIC_API_KEY='...'
octet --safe-mode --model claude-sonnet-4-6
```

Use `/model [id]` to select a model and `/status` to inspect its route and
capabilities. Live discovery is used where a provider exposes it. `--offline`
skips optional discovery, **not inference traffic**.

A pinned, provider-scoped models.dev supplement fills missing **display names,
pricing, input modalities and context/output limits** for models actually
returned by supported built-in discovery. It never supplies tool/structured-output
flags or reasoning controls, and it never replaces a value the endpoint asserts. Endpoint assertions remain authoritative: a live inventory
that asserts any modality field — including an explicit text-only list, or a
false/null/malformed assertion — is honored as-is and the supplement is not
consulted for modalities at all. Only a sparse inventory that says nothing about
input modalities may inherit the snapshot's documented `image`/`audio` input.
Configured/custom metadata and routes keep precedence; Codex account inventory
does not inherit the supplement. Builds and runtime never fetch models.dev. See
the [catalog source and pricing review](../crates/octet-ai/models/SOURCES.md) for
snapshot provenance and the distinction between retained rich records and the
discovery projection.

Input modalities and limits are part of that projection because several providers
publish a sparse model list. Direct DeepSeek is the concrete case: its
`GET /models` returns identifiers only, so before this projection a documented
vision model such as `deepseek-flash` (V4.1 Flash) registered **without** image
input (every attachment failed closed with "Image input is unsupported") and with
the generic 128K/64K placeholder instead of its documented 1M context / 384K
output, silently capping the usable window. Its snapshot record publishes no
price at all, which `models-dev-source.json` marks as an unverified pricing
provider, so DeepSeek spend stays unknown rather than estimated. The
snapshot is consulted per provider and model, so a snapshot entry that declares
text-only input keeps that decision — `deepseek/deepseek-v4-pro` stays
text-only — and a model absent from the snapshot gains no capability.

For sparse direct DeepSeek `deepseek-flash`, the supplement supplies the display
name **DeepSeek V4.1 Flash**, its documented text+image input, and its documented
1M context / 384K output. These apply only because the endpoint asserts none: a
model absent from the snapshot, or an endpoint that publishes its own number,
keeps the generic 128K/64K fallback or the endpoint's value respectively.
Structured-output support is still not taken from the snapshot. Its Off/low/high/max reasoning and native DeepSeek
controls/replay come from the provider-scoped source contract, not models.dev;
explicit endpoint reasoning metadata can narrow or disable that contract. The
separate `deepseek-v4` family keeps its declared 1M/384K limits and
Off/high/xhigh reasoning fallback. Direct DeepSeek's current peak/off-peak tariff
is not modeled: pricing remains unknown unless explicitly configured, so hard
price-dependent ceilings fail closed.

> These are source contracts, not live endpoint verification. Model
> availability remains account- and endpoint-specific; deterministic checks do
> not qualify every live provider.

<a id="endpoint-capability-self-description-unreleased"></a>

## Endpoint capability self-description

An unchanged build can consume new models on **already declared Chat/Responses
routes** when the selected endpoint includes an `octet_capabilities` v1 object in
its ordinary model inventory. Built-in OpenAI-compatible, DeepSeek and OpenRouter
discovery and custom-registry startup use the same bounded decoder. Static-only
providers, native Messages/Google/Bedrock/Conversations routes, Codex and Copilot
retain their existing contracts; this does not create new discovery requests or
bypass a declaration's model filter. Guided `octet setup` does not yet consume
this additional object.

Example entry in `GET /models` (`protocol` uses canonical Rust API spelling):

```json
{
  "id": "future-model",
  "octet_capabilities": {
    "version": 1,
    "protocol": "open_ai_chat",
    "context_window": 131072,
    "max_output_tokens": 16384,
    "input_modalities": ["text", "image"],
    "output_modalities": ["text"],
    "tools": true,
    "parallel_tool_calls": true,
    "structured_output": true,
    "reasoning": {"values": ["none", "low", "high"], "default": "low"}
  }
}
```

`open_ai_responses` is the other supported protocol; it must match the existing
host-selected route. Version, protocol and positive token limits are required;
output cannot exceed context. Omitted capability flags are false, omitted
modalities are text-only, and omitted/null reasoning means no reasoning control.
Reasoning is an exact effort list, not a guessed range; its optional default must
be in the list. The host still selects the provider-specific wire encoding.

Each object is limited to 4096 serialized bytes. Unknown keys/versions, malformed
flags/options, audio or non-text output, parallel calls without tools, and
unsupported protocol declarations fail closed. This schema cannot enable Lite,
Ultra/delegation, deferred tools, native budgets/toggles, arbitrary profiles,
authentication, URLs or transport changes. Explicit legacy endpoint assertions
(including false/null/unknown) win per field; configured model overrides still
win over discovery. No capability is borrowed from models.dev. The decoder's
provenance identifies the host-selected endpoint, returned model and codec,
never an authority or URL claimed by response data.

Built-in raw caches remain URL/account isolated and are decoded on use without
persisting synthesized fields. Custom normalized caches advance to version 9 so
old sparse results cannot hide self-descriptions. These are deterministic source
contracts, not evidence that any public provider currently emits the extension.

<a id="first-run-setup-unreleased"></a>

## First-run setup

When an interactive launch has no available models and no explicit model
selection, the setup menu offers, in order:

1. **Add an API key** — choose a supported built-in provider, paste into a masked
   input, and review before saving. This is a dedicated secret input, not the
   conversation composer; no environment variable is required.
2. **Sign in with ChatGPT / other supported OAuth subscriptions** — choose
   **ChatGPT (OpenAI Codex)** or **GitHub Copilot** and complete the provider's
   device authorization. No other subscription login is implied.
3. **Local/self-hosted models** — choose LM Studio or an explicit
   OpenAI-compatible endpoint, then discover/select a model and review the
   custom registry change.
4. **Continue without a provider** — leave setup without saving provider data.

Existing available models and explicit model selections are not replaced by this
menu. Print/RPC do not open it. Subscription sign-in requires an online launch;
`--offline` is not a local-inference guarantee. After saving a credential, the
catalog is refreshed and model selection uses the ordinary picker. Saving is
not a successful inference check, and a discovery failure can leave the saved
credential in place for retry.

Built-in API keys are saved in
`~/.octet/credentials/api-keys/<provider>.json`, with owner-private directories
(`0700`) and files (`0600`), atomic publication, and explicit consent before
replacement. Environment credentials take precedence over saved keys. Native
provider routes remain native; saving a key does not turn Anthropic, Gemini, or
OpenAI into a custom OpenAI-compatible endpoint.

A saved API key must remain recoverable to authenticate provider requests:
owner-private storage is **not hashing or encryption at rest**. Keep the store
out of repositories, support reports, and shared backups. Keys are not copied to
prompts, configuration, model metadata, or setup receipts. AWS/Bedrock, Azure,
Vertex, and Cloudflare require additional account, deployment, region, or
endpoint configuration and are not offered as one-field API-key setup; use their
documented configuration below.

See [Getting started](getting-started.md#3-choose-one-provider-lane) and
[CLI alternatives](cli.md#provider-setup). This menu is included in octet 0.8.0;
older installations may not provide it.

## Cloud setup

Alternatively, set the credential variables for the chosen row, then run
`octet --model ID`. Do not put credentials into prompts or repository
configuration.

| Provider | Credential and routing setup | Example model ID |
| --- | --- | --- |
| Anthropic | `ANTHROPIC_API_KEY` | `claude-sonnet-4-6` |
| OpenAI | `OPENAI_API_KEY` | `gpt-5.4` or `gpt-6-astra` |
| OpenRouter | `OPENROUTER_API_KEY` | `openrouter/anthropic/claude-sonnet-4.6` |
| Mistral | `MISTRAL_API_KEY`; native Mistral Chat Completions request/reasoning conventions | `mistral/mistral-small-latest` |
| Cloudflare Workers AI | `CLOUDFLARE_ACCOUNT_ID` and `CLOUDFLARE_API_KEY` | `cloudflare-workers-ai/@cf/openai/gpt-oss-120b` |
| Cloudflare AI Gateway | `CLOUDFLARE_ACCOUNT_ID`, non-secret `CLOUDFLARE_GATEWAY_ID`, and `CLOUDFLARE_API_KEY`; documented gateway paths for Claude, OpenAI, and Workers AI | `cloudflare-ai-gateway/claude-sonnet-4-5` |
| Amazon Bedrock | `AWS_REGION` (for example `us-east-1`) or `OCTET_BEDROCK_REGION`; SigV4 bounded AWS credential chain | `bedrock/anthropic.claude-3-7-sonnet-20250219-v1:0` |
| Azure OpenAI | `AZURE_OPENAI_API_KEY`, `AZURE_OPENAI_DEPLOYMENT`, and either `AZURE_OPENAI_RESOURCE` or `AZURE_OPENAI_ENDPOINT` | `azure-openai/my-gpt-deployment` |
| Gemini Developer API | `GEMINI_API_KEY`; native Google `generateContent` | `gemini/gemini-2.5-flash` |
| Vertex AI | ADC, `GOOGLE_CLOUD_PROJECT`, and `GOOGLE_CLOUD_LOCATION` | `vertex/gemini-2.5-flash` |
| Baseten | `BASETEN_API_KEY`; OpenAI Chat | `baseten/<model-id>` |
| Qwen Token Plan | `QWEN_TOKEN_PLAN_API_KEY`; OpenAI Chat | `qwen-token-plan/<model-id>` |
| Qwen Token Plan CN | `QWEN_TOKEN_PLAN_CN_API_KEY`; OpenAI Chat | `qwen-token-plan-cn/<model-id>` |
| Z.AI Coding CN | `ZAI_CODING_CN_API_KEY`; OpenAI Chat | `zai-coding-cn/<model-id>` |

Bedrock accepts an `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` pair with an
optional session token, a web-identity role (`AWS_ROLE_ARN` and
`AWS_WEB_IDENTITY_TOKEN_FILE`, with optional `AWS_ROLE_SESSION_NAME`; one
bounded STS `AssumeRoleWithWebIdentity` exchange, 3 s and 64 KiB, against
`sts.<region>.amazonaws.com` or the `AWS_ENDPOINT_URL_STS` override), the
selected `AWS_PROFILE`, or ECS/EC2 instance metadata — in that order. A
half-configured web identity (only one of the two required variables) fails
closed instead of silently resolving a different identity. A Bedrock API key in
`AWS_BEARER_TOKEN_BEDROCK` is presented as `Authorization: Bearer …` and takes
precedence over SigV4, so an API-key user never needs or pays for the AWS
credential chain.
Model availability depends on account and region. Quote IDs containing shell
metacharacters, for example
`octet --model 'bedrock/anthropic.claude-3-7-sonnet-20250219-v1:0'`.

AWS **instance/container metadata** credentials are *opt-in*, because octet
cannot tell an EC2 instance with a role from an unrelated laptop that would only
time out: probing them on every start costs about a second when no metadata
service is reachable. The metadata sources are consulted only when the local
environment indicates them, and the first matching indication wins:

1. `AWS_EC2_METADATA_DISABLED=false` — the standard AWS switch set to an
   explicit `false`, which allows the metadata sources.
2. `OCTET_AWS_METADATA_CREDENTIALS=1` — octet's explicit opt-in, for an instance
   whose configuration carries no other marker. (`0`, `false`, `no` or `off`
   keep it disabled. An unrecognized value stays closed: unknown state never
   probes.)
3. `AWS_CONTAINER_CREDENTIALS_RELATIVE_URI` / `AWS_CONTAINER_CREDENTIALS_FULL_URI`
   — set by ECS/EKS-style platforms.
4. `AWS_EC2_METADATA_SERVICE_ENDPOINT` / `_MODE` — the standard AWS names — or
   octet's earlier `AWS_METADATA_SERVICE_ENDPOINT` / `_MODE` alias: the host
   pinned the IMDS endpoint.
5. The effective `AWS_PROFILE` declares
   `credential_source = Ec2InstanceMetadata`/`EcsContainer`, or pins IMDS with
   `ec2_metadata_service_endpoint`.
6. The local DMI/SMBIOS markers (`/sys/class/dmi/id/{sys_vendor,board_vendor,
   product_name,bios_vendor}`) name `Amazon EC2`: a bare EC2 instance with an
   instance profile and no other marker. This is a bounded local file read, not
   a metadata request, so it costs a laptop nothing (the files are absent or name
   another hypervisor) and it keeps genuine instance-profile users working
   without an opt-in.

The probe target follows the same order — `AWS_EC2_METADATA_SERVICE_ENDPOINT`,
then `AWS_METADATA_SERVICE_ENDPOINT`, then the profile's
`ec2_metadata_service_endpoint` — and every candidate is validated fail-closed
(plain HTTP is accepted only for loopback or link-local hosts) before a request
is made.

Anything else — including a profile that merely exists, `AWS_PROFILE` alone,
`AWS_CONFIG_FILE`/`AWS_SHARED_CREDENTIALS_FILE` presence, or a DMI vendor that
names another hypervisor — keeps the metadata sources disabled, and an
unrecognized `AWS_EC2_METADATA_DISABLED` value stays
disabled rather than probing on a typo. Static environment keys and profile keys
are unaffected: they are local reads and are always consulted first.

The cost this rule removes is measurable: with an unreachable IMDS, `octet
doctor` was measured at 1,025/1,027 ms, and at 22/19 ms with
`AWS_EC2_METADATA_DISABLED=true` — the difference is the bounded 1 s metadata
timeout. The rule is asserted by **request counts, never millisecond
thresholds** (`crates/octet-coding-agent/src/providers/auth.rs`):
`unrelated_provider_launch_makes_zero_aws_metadata_requests` and
`disabled_activation_opens_no_connection_to_a_live_metadata_endpoint` (a live
loopback listener that drains zero connections) pin the zero-request launch;
`opt_in_activation_resolves_and_signs_with_live_metadata_credentials`,
`an_ec2_instance_identity_still_resolves_instance_credentials`, and
`indicated_metadata_probe_reaches_the_ec2_source` pin that an indicated
EC2/ECS-backed Bedrock run still resolves and signs.

Azure deployments use Responses. A resource can be `my-resource`, or its endpoint
`https://my-resource.openai.azure.com/`; the deployment must name your deployment.
Optional `AZURE_OPENAI_API_VERSION` defaults to the bundled preview version.

Vertex's optional `GOOGLE_APPLICATION_CREDENTIALS` must name an absolute,
owner-private ADC file; otherwise the owner-private default ADC file is checked.
`authorized_user` and PKCS#8 `service_account` ADC files are supported. Access
tokens refresh in memory; octet neither invokes `gcloud` nor persists credential
values. Gemini presets include tools, structured JSON output, and supported images.

Other built-in presets include DeepSeek, Groq, Cerebras, xAI, Together AI,
Fireworks AI, NVIDIA, Hugging Face, Moonshot AI, Xiaomi, MiniMax, and OpenCode Zen.
The [provider declarations](../crates/octet-coding-agent/src/providers/declarations.json)
describe route-specific coverage; a preset name is not a promise of every
provider API.

## Declarative preset metadata

Presets are data, never provider-name branches. The typed preset surface in
`octet_ai::declarations` (`ModelPreset`, `RequestOverrides`,
`ProviderCredentialPreset`, `ChatTemplateValue`) describes the declared
per-model and per-provider fields — `samplingParams`, per-model `headers`,
`vllmPriority`, `supportsMaxOutputTokens`, `thinkingTokenBudgetField`,
`chatTemplateArgs`/`chatTemplateKwargs` with `{ "$var": "thinking.enabled" |
"thinking.effort" | "thinking.budget" }` interpolation, the `string-thinking`
format, and credential environment aliases. Validation is fail-closed: unknown
`$var` names, malformed headers, empty identifiers and unbounded retry/timeout
values are rejected. This is declared plumbing; the OpenAI-compatible codecs and
the streaming client that consume these fields are owned separately, so the
preset validation alone does not establish codec emission or proxy behavior.

Credential aliases recognize Anthropic's `ANTHROPIC_AUTH_TOKEN` and
`ANTHROPIC_OAUTH_TOKEN` (which must be sent as `Authorization: Bearer`) ahead of
`ANTHROPIC_API_KEY` (sent as `x-api-key`), and Vertex's `GOOGLE_CLOUD_API_KEY`.
Route presentation currently follows the declaration's static auth presentation;
selecting bearer-vs-API-key per matched variable is not implemented.

`octet_ai::declarations::proxy` resolves `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY`/
`NO_PROXY` for a request target with upstream root-and-subdomain `NO_PROXY`
semantics (exact host, `.domain`, `*.domain`, optional `:port`, lone `*`), and
rejects non-http(s) proxies. The streaming client does not call this resolver.

## Codex subscription login

```sh
octet --login codex
octet --model gpt-5.6
```

This uses hosted device login instead of a manually managed API key. A successful
account-scoped live inventory is authoritative. octet does not infer Ultra,
collaboration, Responses Lite, or model availability from a name or subscription
plan. Missing or unusable metadata falls back conservatively. If live inventory
omits a model, no corresponding GPT-6 Codex route is injected. When advertised,
select `codex/gpt-6-astra`, `codex/gpt-6-sol`, or `codex/gpt-6-luna`; these stay
namespaced independently of direct OpenAI presets.

Codex discovery sends compatibility version **`0.156.1`**. GPT-6 Sol and Luna
require at least `0.155.0`: older query versions filter them out on the server,
even when the account has access. Read-only checks on 2026-09-23 confirmed that
changing only this query version from `0.153.2`/`0.154.0` to `0.156.1` returned
**`gpt-6-sol`** and **`gpt-6-luna`** with the same OAuth credential. Cache version
8 invalidates the older filtered inventories; the next online launch refreshes
them without another login. Offline launches never perform this refresh.

The observed OAuth contracts include text/image input, a 272K working window,
medium reasoning by default, and low/medium/high/xhigh/max choices; Sol also
advertises Ultra. Both advertise Responses Lite and V2 delegation. All three
GPT-6 routes positively advertise `supports_reasoning_effort_updates`. This
independent capability is retained only with fresh online account metadata;
offline reduction disables it, just like Lite/V2. These are
account-scoped inventory observations, not successful inference checks or claims
about other accounts. Public API `none` support is not imported into the OAuth
choices. Output budgeting and separately sourced prices remain unchanged; the
inventory check did not establish those values. New slugs require account
inventory, not a static alias.

For advertised Ultra/V2 support, first review and activate the subagents source
inside an appropriate OS isolation boundary:

```sh
octet --extension-dir ./extensions \
  --enable-extension octet-subagents --trust-extension octet-subagents \
  --model gpt-5.6-sol --reasoning ultra
```

This requires a live owner-bound child-session service. An installed bundle can
be rebuilt/replaced with `./scripts/reinstall-octet-subagents.sh`; `cargo run`
does not update `~/.octet/extensions`. See the
[subagents package](../extensions/octet-subagents/README.md), including its API
0.4 exact-version boundary. Catalog installation is [publication-gated](installation.md#optional-packages).

<a id="github-copilot-unreleased-candidate"></a>

## GitHub Copilot

```sh
octet --login copilot --headless
octet --logout copilot
```

`github-copilot` is an alias. Login uses GitHub.com's device flow; `--headless`
prints the verification URL/code without opening a browser. Only GitHub OAuth
state is saved in the owner-private `~/.octet/credentials/copilot.json`; inference
tokens stay in memory. No editor, Codex or environment credentials are imported.

Online startup registers authenticated, eligible models as `github-copilot/<id>`
in the ordinary picker and shared native-host catalog. Missing credentials,
failed discovery and unsupported models contribute no Copilot entries.
`--offline` skips even Copilot-store access and advertises no cached inventory.
Only explicit Chat/Responses routes are supported; reasoning-flagged models,
Anthropic-only routes, vision and structured output are not advertised. Custom
Enterprise authorities and environment endpoint overrides are not supported.

Subsequent credential resolution rejects local logout/replacement and rejected
inference origins, but logout does not remotely revoke already-running requests.
First-run setup offers GitHub Copilot device sign-in. The TUI `/login` and
`/logout` slash commands are not yet wired to Copilot; use the CLI flags for
later account changes and restart existing catalog owners afterward. NDJSON
does not gain login/logout commands or OAuth payload fields. Rust embedders retain the
[credential-safe SDK seam](sdk.md#host-owned-github-copilot).

This describes source integration, not live-provider or native-client
qualification.

<a id="native-mistral-conversations-unreleased-codec"></a>

## Native Mistral Conversations (experimental codec)

The native Conversations codec passes its deterministic request/SSE fixtures,
including rejection of credential-bearing or non-TLS destinations before
credential resolution (literal loopback HTTP is allowed for local testing).
Only native completion settles calls: missing `conversation.response.done`
reports `MissingFinish`, never a synthesized tool-call end or successful reply.
No POST is replayed. The built-in Mistral preset above still uses Chat;
Conversations discovery/presets and broader native capabilities remain separate
work, not implied by these codec repairs.

## Local and custom endpoints

Choose **Local/self-hosted models** in [first-run setup](#first-run-setup-unreleased)
for **LM Studio** or an **OpenAI-compatible endpoint**. Choose one endpoint, an
optional credential source, a discovered/manual model ID, and review before
saving. No localhost or network scan occurs. Compatible servers include
llama.cpp, vLLM, SGLang, LM Studio, and compatible gateways.

For scripts, review before adding `--yes`:

```sh
# Explicit LM Studio selection permits its documented default endpoint.
octet setup --preset lm-studio --manual-model local-model

# Discover only this endpoint, then commit.
octet setup --endpoint https://models.example.test/v1/ \
  --api-key-env EXAMPLE_API_KEY --yes

# Manual/offline recovery makes no discovery request.
octet setup --endpoint http://127.0.0.1:8000/v1/ \
  --offline --manual-model Qwen3-Coder --yes
```

`--provider ID` names the custom registry entry; `--label LABEL` sets its display
label. Neither is the model's transport ID. `--model ID` selects discovered
inventory, whereas `--manual-model ID` supplies it manually; the two conflict.
Use `--no-auth` for an explicitly unauthenticated endpoint, or `--api-key-env VAR`
for an environment-backed key, never both. `--replace` permits replacing an
existing provider entry but does not itself commit.

Setup previews the transaction without writing by default. `--yes` confirms it;
`--cancel` cancels it, and the two conflict. Saving uses a private compare-and-swap
(CAS) against the registry snapshot: a concurrent change is rejected, not
overwritten. Review or cancellation can still involve the selected-endpoint
probe; use `--offline --manual-model ID` when no probe is wanted.

Setup's bounded `GET /models` follows no redirects. Receipts, diagnostics,
sessions, and caches never contain API-key/secret-header values; setup writes no
telemetry. `--cancel`, review-only operation, offline failure, or a concurrent
registry change leaves the registry unchanged. Print/RPC modes never open the
guided flow: unresolved models report deterministic `octet setup --yes` recovery.
[All setup argument forms](cli.md#provider-setup).

## Custom registry

Keep custom endpoints together in `~/.octet/credentials/custom.json`, protected
with `chmod 600`. Reference keys through environment variables, not literal values.

```json
{
  "version": 1,
  "providers": {
    "apple-fm": {
      "label": "Apple Foundation Models",
      "base_url": "http://127.0.0.1:1976/v1/",
      "auth": { "kind": "none" },
      "auto_discover": true,
      "startup_timeout_secs": 300,
      "models": [{
        "api_name": "system", "context_window": 8192,
        "max_output_tokens": 1024, "tools": true,
        "parallel_tool_calls": false, "vision": false,
        "structured_output": false, "reasoning": true,
        "reasoning_configurable": false
      }]
    },
    "home-server": {
      "label": "Home Server",
      "base_url": "http://192.168.1.20:8000/v1/",
      "auth": { "kind": "bearer_env", "var": "HOME_SERVER_API_KEY" },
      "auto_discover": true
    },
    "local": {
      "label": "Local Qwen",
      "base_url": "http://127.0.0.1:8000/v1/",
      "auth": { "kind": "none" }, "auto_discover": false,
      "models": [{
        "api_name": "Qwen/Qwen3-Coder-Next", "display_name": "Qwen3 Coder Next",
        "context_window": 131072, "max_output_tokens": 16384,
        "tools": true, "parallel_tool_calls": false, "vision": false,
        "structured_output": false, "reasoning": true,
        "reasoning_values": ["none", "default"], "reasoning_default": "default"
      }]
    }
  }
}
```

Each provider is discovered independently. Stable IDs are
`custom/<provider-id>/<model-id>`; labels appear in the picker and `/status`.
Configured model metadata overrides matching discovery results. Use
`auto_discover: false` with an explicit `models` inventory if `GET /v1/models`
is not useful. Legacy single-object files normalize in memory to `custom-openai`
without breaking existing IDs; new files should use the versioned registry above.

Apple Foundation Models supplies sparse metadata: keep `system` at 8192 context
tokens, `reasoning: true`, and `reasoning_configurable: false`. It thinks by
default and offers only `on`, not configurable `reasoning_effort`. Its separate
`pcc` model has a 32768-token window and low/medium/high effort. When `fm serve`
is not running, octet skips this optional loopback `GET /v1/models` without a
connection warning. Example limits above are model metadata, not global defaults.

Custom models receive trusted zero pricing for cost guardrails, so local and
self-hosted models can use price-dependent features such as subagents. To track
spend, supply per-model rates in **microdollars per million tokens**; omitted
rates remain zero:

```json
{"api_name":"metered-model","pricing":{"input":75,"output":300,"cache_read":8,"cache_write_5m":19}}
```

## Cold-start feedback

Set `lifecycle_feedback: true` on a custom provider only if its streaming Chat
Completions endpoint implements the optional readiness extension. octet then
sends `x-octet-lifecycle: 1`; the endpoint may return that header and/or comments
such as `: octet-lifecycle: loading; warming model`. Accepted states are `queued`,
`loading`, and `ready`; malformed values and ordinary SSE comments stay invisible.
Unconfigured endpoints receive no header; ordinary OpenAI clients ignore it.

Feedback is bounded, redacted, transient status—not assistant content, session
history, or model context. Plain/print write it to stderr, leaving print stdout
response-only. It adds no retries, accepted-POST replay, or special `503` handling.
`startup_timeout_secs` still bounds response headers; feedback cannot extend it.
Ordinary body idle/deadline limits apply after a successful stream starts.
Non-streaming requests neither negotiate nor emit feedback. See the
[transport contract](design/octet-ai.md#opt-in-endpoint-lifecycle-feedback).

## Reasoning

```sh
octet --reasoning high
octet --reasoning budget=16000
```

`budget=N` is available only for compatible models. `/thinking [level]` offers
`off`, `on`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`, or `ultra`, narrowed
to the selected model. Exact off-only, binary, or named custom controls determine
the picker and wire values, rather than a generic effort guess.

Built-in native discovery may use a declaration-owned token-budget table only
when the entire table fits strictly below the effective output ceiling. Otherwise
the inventory model keeps its limits but advertises no reasoning control; octet
does not enlarge the ceiling or invent replacement budgets from the snapshot.
Compatible endpoint choices/defaults can narrow the declared contract without
changing its native codec.

`ultra` requires advertised Ultra/V2 metadata **and** the trusted, enabled,
live `octet-subagents` service. Otherwise it is clamped to the highest ordinary
safe effort. Child work uses extension `subagent_*` tools and `/subagents`;
there is no parallel native root collaboration tool surface. See
[legacy Pro configuration](configuration.md#compatibility-inputs),
[context budgeting](context.md), and [reasoning display](terminal.md#reasoning-and-progress).

<a id="defaults-unreleased"></a>

### Defaults

In this checkout, a new CLI session with no reasoning preference uses the selected
model's advertised default, including an advertised Off. Without a default, a
known reasoning contract uses its first supported enabled choice; no usable
contract leaves octet's selection Off without guessing a reasoning parameter.
The same defaults appear in Serve's model catalog. This fixes startup overriding
model defaults with Off in octet 0.8.0.

An explicit CLI, environment, or configuration choice still wins over the model
default. Resume keeps the saved choice (including Off), unless `--reasoning`
overrides it. Existing saved Off values are not automatically reinterpreted as
unset. In interactive octet, submit `/thinking on` or another supported level to
change and persist that session's selection; inspect `/status` afterward.

A custom `none/default` contract's On leaves the reasoning control absent so the
server uses its default; always-on models also receive no control parameter.
Unknown metadata and a lack of displayed reasoning do **not** establish that the
server has disabled thinking. Reasoning can increase latency and token usage;
use `--reasoning off` when the model supports it to opt out.

The OpenRouter route decodes that provider's own per-model `reasoning` object
(`mandatory`, `default_enabled`, `supported_efforts`, `default_effort`) rather
than inferring optionality from the mere presence of a `reasoning_effort`
parameter. Mandatory models offer only their advertised efforts (for GLM 5.3,
`max/high/low`, default `max`), or only `on` if no efforts are published. A saved
unsupported `off` is normalized to an advertised choice with a diagnostic.
Local summaries use that same contract: Off only when supported, otherwise the
advertised default. A parameter list without an exact contract offers only the
endpoint default, never a guessed `none/minimal/low/medium/high` range.

For the OpenRouter profile, `off` **omits** the reasoning object rather than
sending `effort: "none"` or `enabled: false`; the endpoint may therefore still
reason according to its default. Enabled efforts are sent verbatim; an enabled
boolean-only contract sends `enabled: true`. Other providers retain their
existing explicit-disable behavior. This is a wire-compatibility policy, not a
guarantee that selecting Off disables reasoning on OpenRouter.
See [reasoning selection and thinking](provider-thinking.md).

## Protocols and transport

| Protocol | Streaming | Tools | Reasoning | Images | Structured output |
| --- | :---: | :---: | :---: | :---: | :---: |
| OpenAI Responses | Yes | Yes | Yes | Yes | Yes |
| OpenAI Chat Completions | Yes | Yes | Yes | Yes | Yes |
| Anthropic Messages | Yes | Yes | Yes | Yes | Yes |
| Amazon Bedrock Converse | Yes | Yes | Capability/model-dependent token thinking | Yes | No |

Capabilities are model-specific and validated before submission, including
modalities, tools, structured output, output limits, and reasoning. Google uses
native [generateContent](design/octet-ai.md#google-generatecontent), not an OpenAI
translation; protocol recognition alone does not imply [native audio support](media.md#formats-and-limits).

Direct OpenAI and Codex declare `WebSocketPreferred` with HTTP/SSE fallback
for ordinary requests. Native steering requires its bidirectional WebSocket
operation and does not replay accepted input through HTTP. Codex additionally
uses endpoint-configured zstd HTTP request compression;
compression failure keeps the valid uncompressed body. These are provider
[declarations](../crates/octet-coding-agent/src/providers/declarations.json),
not automatic properties of the Responses codec.

Responses Lite applies to ordinary and native compact requests: it sends the
Lite header, tool schemas and developer instructions as input items, reasoning
context across all turns, and no unsupported image-detail hints. **Lite sends
`parallel_tool_calls: false` even for parallel-capable models.** Exact shapes
remain in the [Lite contract](design/octet-ai.md#responses-lite) and
[wire fixtures](../crates/octet-ai/src/protocol/openai_responses.rs).
Only explicitly parallel-safe pure/workspace-read effects overlap; shell and
mutation effects remain serialized regardless of model batching.

The Responses `service_tier` request field (`auto`, `default`, `flex`,
`priority`) is a declared endpoint capability, not a provider identity: the
codec sends it only when the caller selects one **and** the route's declared
`runtime.responses_profile` accepts it (`Codex` today, the same gate `/fast`
uses). Any other profile fails closed with a typed unsupported error instead of
silently dropping a caller's billing-changing control. Cost settlement and
conservative request reservations use the declared Codex
tier tariff with the provider's echoed tier: flex is one-half, priority is twice
the base rate (five-halves for exact API model `gpt-5.5`). Unknown tiers,
unresolved `auto`, and unsupported tariffs remain unpriced. The priority
uncertainty marker stays durable; tier settlement does not clear prior exposure.

The OpenAI Responses **computer-use tool**
(`{"type":"computer_use_preview","display_width":…,"display_height":…,
"environment":…}`) is likewise a declared capability: a caller must select it
(`ResponsesOptions::with_computer_use`) **and** the route's declared
`runtime.responses_profile` must accept it (the public Responses profile today;
any other profile fails closed with a typed unsupported error). Octet carries
the protocol only — it declares the tool, maps a provider `computer_call` to a
canonical tool call named `computer_use_preview` with a bounded action payload
(`click`, `double_click`, `drag`, `keypress`, `move`, `screenshot`, `scroll`,
`type`, `wait`; anything else fails closed, including a missing action), and
maps the caller's result back to a `computer_call_output` item carrying the one
documented `computer_screenshot` object. **Nothing in octet executes a computer
action**: there is no desktop or browser backend behind this codec, and whether
any action may run is a separate host-policy decision.

`previous_response_id` is a best-effort process-local live-WebSocket optimization
only when fixed parameters and the prior input/output prefix match. It is not
a durable cursor: resume or mismatch uses full local replay; native Responses
uses persisted route-affine opaque replay. The
[WebSocket implementation](../crates/octet-ai/src/responses_ws.rs) and
[recovery boundary](tools.md#recovery-and-security) distinguish a recognized
pre-generation connection-lifetime rejection (socket retirement, safe HTTP retry)
from an accepted POST or body disconnect. By default, ambiguous accepted requests
are not replayed, even before visible output. The agent has a narrow exception
for host-qualified Codex Responses requests (including Lite) with local function
tools: before assistant commit it may discard provisional output and replace
interrupted inference from durable context. This can duplicate remote generation
and incur unknown charges; it does not replay committed local tool effects or
prove remote cancellation. The agent separates finite streamed-inference
replacement and HTTP-admission retry budgets, without resetting on transport
fallback.
These bound attempts, not confirmed accepted generations or charges; eligibility
and hard ceilings can stop recovery earlier. Only
positively classified pre-send outages enter sustained, cancellable network
waiting. Unknown failed-attempt usage blocks replacement under hard cumulative
cost/token ceilings. Unknown exposure is durably recorded independently of known
usage: later success, resume, or checkout does not restore complete totals.
Displayed numeric usage/cost is then a known subtotal; a fork starts independent
accounting. See the [agent recovery contract](design/octet-agent.md#in-process-provider-recovery)
for eligibility, budgets, and verification limits.

## GPT-6 contracts and execution

Direct `gpt-6-astra`, `gpt-6-sol`, and `gpt-6-luna` are declared on Responses with
text/image input, a 1.05M-token context window and 128K output. Astra accepts
`low` through `max` (default low); Sol/Luna additionally accept Off/`none` and
default to medium. Exact public pricing and its above-272K tier are recorded in
[the catalog sources](../crates/octet-ai/models/SOURCES.md). New public prices
are not borrowed for Codex Sol/Luna: their subscription cost remains unknown.

Model and endpoint feature declarations must **both** opt in:

- **Async tools:** qualified host-parallel observations advertise `async: true`.
  Complete calls are persisted before a bounded background job starts; the model
  can advance while those jobs run, and each result uses its original call ID.
  This is not speculative execution of streamed arguments. Synchronous calls,
  effectful work, approvals, and hard usage ceilings retain their barriers.
  Interrupted pending work is not blindly rerun after restart.
- **Native steering:** public GPT-6 uses `response.steer` on the active socket,
  with durable admission before dispatch and separate accounting/persistence for
  every response segment. Provider acceptance is not proof that an instruction
  was followed. Ambiguous disconnects are not automatically replayed. Routes or
  input forms without qualification retain ordinary queued steering; Codex does
  not gain native steering from its model name. Hard cumulative ceilings retain
  the ordinary queue boundary.
- **Thinking changes:** qualified `/thinking` changes queue for the next response
  boundary without cancelling the root run. The request-level reasoning baseline
  stays fixed and ordered `configuration_update` items carry ordinary effort
  changes. Off still requires an advertised `none`; Ultra/delegation-mode changes
  are not ordinary wire effort updates. Durable replay retains the baseline and
  effective selection. Compaction must rebase only after a successful summary;
  standalone native compact rejects update histories without separate authority.

Codex currently qualifies reasoning updates from positive account inventory, not
async tools or native steering. Lite/V2 do not imply either feature. Public API
support and deterministic loopback tests are not live OAuth inference or cache-hit
qualification. See [reasoning controls](provider-thinking.md) and
[the protocol contract](../crates/octet-ai/docs/responses-controls.md).

Third-party GPT-6 inventory does not inherit these capabilities from a name or
snapshot. OpenRouter must advertise its own images, tools and exact reasoning
choices; its ordinary Chat route does not become a Responses control endpoint.
Provider-managed misalignment monitoring remains independent of host approval:
`misalignment_policy_violation` stops automatic retry, and already completed
work is not undone. Octet does not provision project webhooks or safety-alert
subscriptions automatically.
