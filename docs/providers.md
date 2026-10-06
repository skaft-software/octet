# Providers and models

[Documentation](README.md) · [Configuration](configuration.md) · [Media](media.md)

```sh
export ANTHROPIC_API_KEY='...'
octet --safe-mode --model claude-sonnet-4-6
```

`/model [id]` picks a model, and `/status` shows its route and capabilities.
octet uses live model discovery where the provider offers it. `--offline` skips
optional discovery, **not inference traffic**. Which models you can use depends
on your account and endpoint, and octet's tests don't cover every live provider.

New here? Start with [first-run setup](#first-run-setup-unreleased), or set your
provider's variable from [cloud setup](#cloud-setup).

<a id="first-run-setup-unreleased"></a>

## First-run setup

When an interactive launch has no available models and no explicit model
selection, the setup menu offers, in order:

1. **Add an API key.** Choose a supported built-in provider, paste into a masked
   input and review before saving. This is a dedicated secret input, not the
   conversation composer, and no environment variable is required.
2. **Sign in with ChatGPT / other supported OAuth subscriptions.** Choose
   **ChatGPT (OpenAI Codex)** or **GitHub Copilot**. Codex offers browser
   sign-in (PKCE) or a device code, and Copilot uses device authorization. No
   other subscription login is implied.
3. **Local/self-hosted models.** Choose LM Studio or an explicit
   OpenAI-compatible endpoint, then discover or select a model and review the
   custom registry change.
4. **Continue without a provider.** Leave setup without saving provider data.

Existing available models and explicit model selections don't trigger this menu.
In the TUI, `/setup` opens the same wizard on demand, even with a configured
provider. A saved provider refreshes the model catalog but keeps your current
session, active model and default model, so use `/model` to switch. If you
replace the active route's stored key, reselect that model to rebuild its
credential. A credential environment variable still takes precedence, and the
wizard warns before saving a fallback key. Print and RPC don't open the wizard.
Subscription sign-in needs an online launch, and `--offline` isn't a
local-inference guarantee. After a credential is saved the catalog refreshes and
model selection uses the ordinary picker. Saving isn't a successful inference
check, and a discovery failure can leave the saved credential in place for a
retry.

Built-in API keys are saved in `~/.octet/credentials/api-keys/<provider>.json`,
with owner-private directories (`0700`) and files (`0600`), atomic publication
and explicit consent before replacement. Environment credentials take precedence
over saved keys. Native provider routes stay native: saving a key doesn't turn
Anthropic, Gemini or OpenAI into a custom OpenAI-compatible endpoint.

A saved API key has to stay recoverable to authenticate provider requests, so
owner-private storage is **not hashing or encryption at rest**. Keep the store
out of repositories, support reports and shared backups. Keys aren't copied to
prompts, config, model metadata or setup receipts. AWS/Bedrock, Azure, Vertex
and Cloudflare need extra account, deployment, region or endpoint configuration
and aren't offered as one-field API-key setup, so use their documented
configuration below.

See [Getting started](getting-started.md#3-choose-one-provider-lane) and the
[CLI alternatives](cli.md#provider-setup). This setup flow is part of the current
0.9.0 source candidate; availability in older published versions differs.

## Cloud setup

Alternatively, set the credentials for your provider, then run
`octet --model ID`. Don't put credentials in prompts or repository config.

| Provider | Set | Example model ID |
| --- | --- | --- |
| Anthropic | `ANTHROPIC_API_KEY` | `claude-sonnet-4-6` |
| OpenAI | `OPENAI_API_KEY` | `gpt-5.4` or `gpt-6-astra` |
| OpenRouter | `OPENROUTER_API_KEY` | `openrouter/anthropic/claude-sonnet-4.6` |
| Mistral | `MISTRAL_API_KEY` (native Mistral Chat Completions request and reasoning conventions) | `mistral/mistral-small-latest` |
| Cloudflare Workers AI | `CLOUDFLARE_ACCOUNT_ID`, `CLOUDFLARE_API_KEY` | `cloudflare-workers-ai/@cf/openai/gpt-oss-120b` |
| Cloudflare AI Gateway | `CLOUDFLARE_ACCOUNT_ID`, `CLOUDFLARE_GATEWAY_ID` (not secret), `CLOUDFLARE_API_KEY`. Gateway paths for Claude, OpenAI and Workers AI. | `cloudflare-ai-gateway/claude-sonnet-4-5` |
| Amazon Bedrock | `AWS_REGION` (such as `us-east-1`) or `OCTET_BEDROCK_REGION`, plus AWS credentials (SigV4) | `bedrock/anthropic.claude-3-7-sonnet-20250219-v1:0` |
| Azure OpenAI | `AZURE_OPENAI_API_KEY`, `AZURE_OPENAI_DEPLOYMENT`, and `AZURE_OPENAI_RESOURCE` or `AZURE_OPENAI_ENDPOINT` | `azure-openai/my-gpt-deployment` |
| Gemini Developer API | `GEMINI_API_KEY` (native Google `generateContent`) | `gemini/gemini-2.5-flash` |
| Vertex AI | ADC, `GOOGLE_CLOUD_PROJECT`, `GOOGLE_CLOUD_LOCATION` | `vertex/gemini-2.5-flash` |
| Baseten | `BASETEN_API_KEY` (OpenAI Chat) | `baseten/<model-id>` |
| Qwen Token Plan | `QWEN_TOKEN_PLAN_API_KEY` (OpenAI Chat) | `qwen-token-plan/<model-id>` |
| Qwen Token Plan CN | `QWEN_TOKEN_PLAN_CN_API_KEY` (OpenAI Chat) | `qwen-token-plan-cn/<model-id>` |
| Z.AI Coding CN | `ZAI_CODING_CN_API_KEY` (OpenAI Chat) | `zai-coding-cn/<model-id>` |

The built-in OpenRouter route sends fixed app-attribution defaults on model
discovery, Chat Completions and Batch API requests:

| Header | Default value |
| --- | --- |
| `HTTP-Referer` | `https://octet.skaft.org` |
| `X-OpenRouter-Title` | `octet coding agent` |
| `X-OpenRouter-Categories` | `cli-agent` |

These headers identify the app for [OpenRouter's public rankings and app
analytics](https://openrouter.ai/docs/app-attribution). They contain no user,
project or session identifiers and add no separate telemetry request. The same
identity is used whether the credential comes from the environment or saved
setup. Other built-in providers and custom OpenAI-compatible endpoints don't
inherit these defaults. Explicit model and request header overrides keep their
normal precedence. Attribution applies to new requests, not past usage.

Other built-in presets: DeepSeek, Groq, Cerebras, xAI, Together AI, Fireworks
AI, NVIDIA, Hugging Face, Moonshot AI, Xiaomi, MiniMax and OpenCode Zen. A
preset name doesn't promise every API of that provider. The [provider
declarations](../crates/octet-coding-agent/src/providers/declarations.json) list
what each route covers.

Bedrock takes AWS credentials in this order: an `AWS_ACCESS_KEY_ID` /
`AWS_SECRET_ACCESS_KEY` pair (with an optional session token), a web-identity
role, the selected `AWS_PROFILE`, then ECS or EC2 instance metadata. A Bedrock
API key in `AWS_BEARER_TOKEN_BEDROCK` is sent as `Authorization: Bearer …` and
takes precedence over SigV4, so an API-key user never needs or pays for the AWS
credential chain. Model availability depends on your account and region. Quote
IDs that contain shell characters:
`octet --model 'bedrock/anthropic.claude-3-7-sonnet-20250219-v1:0'`.

<details>
<summary>Bedrock web identity and instance metadata</summary>

A web-identity role uses `AWS_ROLE_ARN` and `AWS_WEB_IDENTITY_TOKEN_FILE`, with
an optional `AWS_ROLE_SESSION_NAME`. It does one bounded STS
`AssumeRoleWithWebIdentity` exchange (3 s and 64 KiB) against
`sts.<region>.amazonaws.com` or the `AWS_ENDPOINT_URL_STS` override. A
half-configured web identity (only one of the two required variables) fails
closed instead of silently resolving a different identity.

AWS **instance and container metadata** credentials are *opt-in*, because octet
can't tell an EC2 instance with a role from an unrelated laptop that would only
time out. Probing them on every start costs about a second when no metadata
service is reachable. The metadata sources are consulted only when the local
environment indicates them, and the first matching indication wins:

1. `AWS_EC2_METADATA_DISABLED=false`: the standard AWS switch, set to an
   explicit `false`, which allows the metadata sources.
2. `OCTET_AWS_METADATA_CREDENTIALS=1`: octet's explicit opt-in, for an instance
   whose configuration carries no other marker. (`0`, `false`, `no` or `off`
   keep it disabled. An unrecognized value stays closed: unknown state never
   probes.)
3. `AWS_CONTAINER_CREDENTIALS_RELATIVE_URI` or
   `AWS_CONTAINER_CREDENTIALS_FULL_URI`, set by ECS and EKS-style platforms.
4. `AWS_EC2_METADATA_SERVICE_ENDPOINT` or `_MODE` (the standard AWS names), or
   octet's earlier `AWS_METADATA_SERVICE_ENDPOINT` or `_MODE` alias: the host
   pinned the IMDS endpoint.
5. The effective `AWS_PROFILE` declares
   `credential_source = Ec2InstanceMetadata` or `EcsContainer`, or pins IMDS
   with `ec2_metadata_service_endpoint`.
6. The local DMI/SMBIOS markers
   (`/sys/class/dmi/id/{sys_vendor,board_vendor,product_name,bios_vendor}`) name
   `Amazon EC2`: a bare EC2 instance with an instance profile and no other
   marker. This is a bounded local file read, not a metadata request, so it
   costs a laptop nothing (the files are absent or name another hypervisor), and
   it keeps genuine instance-profile users working without an opt-in.

The probe target follows the same order (`AWS_EC2_METADATA_SERVICE_ENDPOINT`,
then `AWS_METADATA_SERVICE_ENDPOINT`, then the profile's
`ec2_metadata_service_endpoint`), and every candidate is validated fail-closed
(plain HTTP is accepted only for loopback or link-local hosts) before a request
is made.

Anything else keeps the metadata sources disabled: a profile that merely exists,
`AWS_PROFILE` alone, `AWS_CONFIG_FILE` or `AWS_SHARED_CREDENTIALS_FILE`
presence, or a DMI vendor that names another hypervisor. An unrecognized
`AWS_EC2_METADATA_DISABLED` value stays disabled rather than probing on a typo.
Static environment keys and profile keys are unaffected: they're local reads and
are always consulted first.

The cost this rule removes is measurable. With an unreachable IMDS,
`octet doctor` was measured at 1,025/1,027 ms, and at 22/19 ms with
`AWS_EC2_METADATA_DISABLED=true`, and the difference is the bounded 1 s metadata
timeout. The rule is asserted by **request counts, never millisecond
thresholds** (`crates/octet-coding-agent/src/providers/auth.rs`).
`unrelated_provider_launch_makes_zero_aws_metadata_requests` and
`disabled_activation_opens_no_connection_to_a_live_metadata_endpoint` (a live
loopback listener that drains zero connections) pin the zero-request launch.
`opt_in_activation_resolves_and_signs_with_live_metadata_credentials`,
`an_ec2_instance_identity_still_resolves_instance_credentials` and
`indicated_metadata_probe_reaches_the_ec2_source` pin that an indicated EC2 or
ECS-backed Bedrock run still resolves and signs.

</details>

<details>
<summary>Azure and Vertex details</summary>

**Azure.** Deployments use Responses. The resource can be `my-resource` or its
endpoint, `https://my-resource.openai.azure.com/`. The deployment must name your
deployment. `AZURE_OPENAI_API_VERSION` is optional and defaults to the bundled
preview version.

**Vertex.** The optional `GOOGLE_APPLICATION_CREDENTIALS` must name an absolute,
owner-private ADC file. Otherwise octet checks the owner-private default ADC
file. `authorized_user` and PKCS#8 `service_account` files work. Access tokens
refresh in memory, and octet neither runs `gcloud` nor stores credential values.
Gemini presets support tools, structured JSON output and images.

</details>

## Codex subscription login

```sh
octet --login codex
octet --model gpt-5.6
```

Codex offers **Sign in with your browser (recommended)** and **Use a device code
(SSH/headless)**, instead of a manually managed API key. Browser sign-in opens
the OpenAI authorization page, also prints its URL, and listens only on
`127.0.0.1:1455` (or the registered fallback port `1457` when 1455 is busy) for
up to five minutes. The browser shows a self-contained signed-in page once the
credential is saved. `--headless`, SSH, an unavailable browser opener, or both
callback ports being busy uses the hosted device-code flow, and you can choose
it explicitly too.

A successful account-scoped live inventory is authoritative. octet asks your
account which models you can use, and doesn't guess Ultra, collaboration,
Responses Lite or model availability from a name or plan. If the metadata is
missing or unusable, octet falls back to the more limited behavior. If the live
inventory omits a model, no corresponding GPT-6 Codex route is injected. When
advertised, select `codex/gpt-6.1-sol`, `codex/gpt-6-astra`, `codex/gpt-6-sol`
or `codex/gpt-6-luna`. These stay namespaced separately from direct OpenAI
presets. When discovery is unreachable, 6.1 Sol leads the fallback suggestions.

<details>
<summary>Discovery version, GPT-6 account contracts and pricing</summary>

Codex discovery sends compatibility version **`0.159.2`**. GPT-6 Sol and Luna
need at least `0.155.0`: older query versions filter them out on the server,
even when the account has access. Read-only checks on 2026-09-23 confirmed that
changing only this query version from `0.153.2`/`0.154.0` to `0.156.1` returned
**`gpt-6-sol`** and **`gpt-6-luna`** with the same OAuth credential. Cache
version 8 invalidates the older filtered inventories, and the next online launch
refreshes them without another login. Offline launches never perform this
refresh.

The 2026-09-23 OAuth contracts include text and image input, a 272K working
window, medium reasoning by default, and low, medium, high, xhigh and max
choices. Sol also advertises Ultra, and both advertise Responses Lite and V2
delegation. All three GPT-6 routes positively advertise
`supports_reasoning_effort_updates`. That independent capability is kept only
with fresh online account metadata, and offline reduction disables it, just like
Lite and V2. These are account-scoped inventory observations, not successful
inference checks or claims about other accounts. Public API `none` support isn't
imported into the OAuth choices. Output budgeting and separately sourced prices
are unchanged, because the inventory check didn't establish those values. New
slugs need account inventory, not a static alias.

The Codex 6.1 Sol bundled catalog advertises a 272K default and 872K maximum
context, low reasoning by default with low through Ultra, Lite/V2 and
priority 1. Offline fallback removes dynamic Ultra/V2. That catalog lists a
0.153.0 minimum, but the live backend withheld 6.1 Sol from the 0.156.1 query
version while Codex 0.159.2 listed it for the same account. Discovery therefore
sends 0.159.2, and cache schema 10 refreshes older inventories. Its subscription
price stays unknown.

</details>

<details>
<summary>Ultra/V2 reasoning with subagents</summary>

For models that advertise Ultra/V2, first review and enable the subagents
source, inside an OS isolation boundary you trust:

```sh
octet --extension-dir ./extensions \
  --enable-extension octet-subagents --trust-extension octet-subagents \
  --model gpt-5.6-sol --reasoning ultra
```

This needs a live, owner-bound child-session service. To rebuild or replace an
installed bundle, run `./scripts/reinstall-octet-subagents.sh`. `cargo run`
doesn't update `~/.octet/extensions`. See the [subagents
package](../extensions/octet-subagents/README.md) and its API 0.4 exact-version
boundary. Catalog install works once the matching release is published:
[Optional packages](installation.md#optional-packages).

</details>

<a id="subscription-oauth-logins"></a>

## Subscription OAuth logins

Beyond Codex, octet signs in to paid plans directly rather than asking for a
long-lived API key. Each provider gets its own owner-private credential file
under `~/.octet/credentials/`, and its models appear in the catalog only while
that file says you are signed in.

| Command | Plan | Flow |
| --- | --- | --- |
| `octet --login grok` | SuperGrok or X Premium | xAI device code |
| `octet --login kimi` | Kimi Code | Kimi device code |
| `octet --login meta` | Meta Muse | Meta device code, then an API-key mint |
| `octet --login openrouter` | OpenRouter account | PKCE in a browser, pasted redirect |

These are separate providers from the same vendors' API-key presets, so signing
in with a plan never replaces a key you already configured:

| Plan | API key |
| --- | --- |
| `xai-subscription/<model>` | `xai/<model>` |
| `kimi-coding-subscription/<model>` | `kimi-coding/<model>` |
| `meta-subscription/<model>` | `meta/<model>` |
| `openrouter-oauth/<model>` | `openrouter/<model>` |

`--headless` prints the verification URL and code without opening a browser, so
the device flows work over SSH. `--logout <provider>` removes only that
provider's credential and never contacts the provider; for the plans, revoke
access from the vendor's own account page.

Meta is worth calling out because its grant is two steps. The device flow yields
an identity token that the inference surface will not accept, so octet
immediately exchanges it for a short-lived Model API key. The identity token is
what makes the exchange repeatable; when Meta rejects it, the session is over and
octet asks you to sign in again rather than retrying. OpenRouter is the opposite:
its login mints a durable API key with no refresh token, so octet never renews it
and `--logout` only deletes the local copy. Its authorization URL is printed even
when browser launch fails or an opener fails to settle promptly. Opener children
are bounded and reaped; waiting for a pasted redirect is cancellable without
leaving a background stdin reader behind.

Credentials are refreshed automatically. A refresh is serialized both within one
octet process and across octet processes, and the credential file is re-read
after the cross-process lock is taken, so two concurrent launches cannot spend
the same single-use refresh token and leave you signed out. Once refresh work
starts, it owns the exchange, both locks and persistence independently of the
request waiting for it: cancelling that wait does not abandon an issued rotated
token. This does not promise recovery from process termination, runtime shutdown
or an indeterminate provider response. A provider that rotates its refresh token
but does not return a new one is treated as a failure
and the existing credential is left untouched, because that provider has already
revoked it.

### Not yet available

- **Anthropic (Claude Pro/Max).** The login is a PKCE grant rather than a device
  code, which octet does not yet support end to end. It also needs a request-path
  change: the Anthropic Messages codec decides whether to send the
  `claude-code-*` and `oauth-*` beta headers from the credential that is bound to
  the route, and it currently recognizes only an environment bearer token. A
  privately resolved OAuth credential therefore would not receive the betas a
  subscription token needs. Fixing that means teaching the codec about a
  dynamically resolved Anthropic route, which is a change to `octet-ai` and is
  tracked separately rather than approximated here. `ANTHROPIC_AUTH_TOKEN` and
  `ANTHROPIC_OAUTH_TOKEN` continue to work today.
- **Radius.** Its login is defined against a Pi-hosted gateway that also serves
  Pi's own wire protocol. octet has no such codec, and adding one is a separate
  change.

<a id="github-copilot-unreleased-candidate"></a>

## GitHub Copilot

```sh
octet --login copilot --headless
octet --logout copilot
```

`github-copilot` is an alias. Login uses GitHub.com's device flow, and
`--headless` prints the verification URL and code without opening a browser.
Only GitHub OAuth state is saved, in the owner-private
`~/.octet/credentials/copilot.json`. Inference tokens stay in memory, and no
editor, Codex or environment credentials are imported.

Online startup registers authenticated, eligible models as `github-copilot/<id>`
in the ordinary picker and the shared native-host catalog. Missing credentials,
failed discovery and unsupported models contribute no Copilot entries.
`--offline` skips even Copilot-store access and advertises no cached inventory.
Only explicit Chat and Responses routes are supported. Reasoning-flagged models,
Anthropic-only routes, vision and structured output aren't advertised. Custom
Enterprise authorities and environment endpoint overrides aren't supported.

Later credential resolution rejects local logout or replacement and rejected
inference origins, but logout doesn't remotely revoke already-running requests.
First-run setup offers GitHub Copilot device sign-in. The TUI `/login` and
`/logout` slash commands aren't wired to Copilot yet, so use the CLI flags for
later account changes and restart existing catalog owners afterward. NDJSON
gains no login or logout commands or OAuth payload fields. Rust embedders keep
the [credential-safe SDK seam](sdk.md#host-owned-github-copilot). This describes
source integration, not live-provider or native-client qualification.

## Local and custom endpoints

Choose **Local/self-hosted models** in [first-run
setup](#first-run-setup-unreleased) for **LM Studio** or an **OpenAI-compatible
endpoint**. You choose one endpoint, an optional credential source and a
discovered or manual model ID, then review before saving. octet never scans
localhost or the network. Compatible servers include llama.cpp, vLLM, SGLang, LM
Studio and compatible gateways.

For scripts, review first, then add `--yes`:

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

Every flag is in the [CLI reference](cli.md#provider-setup).

Setup writes nothing until you confirm. Even a review or cancel can probe the
selected endpoint, so use `--offline --manual-model ID` when you want no probe.

<details>
<summary>What setup guarantees</summary>

Saving compares against the registry snapshot taken at review, and rejects a
concurrent change instead of overwriting it. The probe (`GET /models`) follows
no redirects. Receipts, diagnostics, sessions and caches never contain API key
or secret header values, and setup writes no telemetry. A cancel, a review-only
run, an offline failure or a concurrent change leaves the registry unchanged.
Print and RPC modes never open the guided flow: an unresolved model reports the
`octet setup --yes` recovery.

</details>

## Custom registry

Keep custom endpoints together in `~/.octet/credentials/custom.json`, with
`chmod 600`. Reference keys through environment variables, never literal values.

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

Each provider is discovered on its own. IDs look like
`custom/<provider-id>/<model-id>`, and labels show in the picker and `/status`.
Set `auto_discover: false` with an explicit `models` list to pin the registry as
truth when `GET /v1/models` isn't useful. Old single-object files are read as
`custom-openai` without changing existing IDs. New files should use the
versioned registry above.

For providers with `auto_discover: true`, limits the endpoint asserts are
authoritative, and the registry is a seed and fallback. Every other registry
field (display name, tools and vision flags, reasoning values, pricing, presets)
keeps configured-wins behavior.

<details>
<summary>How asserted limits and the registry combine</summary>

A live `max_model_len` (or `context_window` / `context_length`) assertion wins
over a stale registry `context_window` pin. `max_output_tokens` is the tighter
of the endpoint and registry caps, clamped to the live window, so a vLLM profile
switch in either direction is followed without editing the registry. Only an
*asserted* limit is authoritative. A sparse `/v1/models` response that publishes
ids and nothing else asserts neither limit, so a configured `context_window` and
`max_output_tokens` survive discovery unchanged instead of being replaced by
octet's discovery fallbacks, and a model with no configured counterpart keeps
the fallback. The two limits are recorded independently, so an endpoint that
reports its served context but no output cap follows the live window while the
configured output cap survives. Cached inventories carry that provenance, so the
offline and online paths resolve to the same effective limits.

</details>

Custom models count as free for cost guardrails, so local and self-hosted models
can use price-dependent features such as subagents. To track spend, give
per-model rates in **microdollars per million tokens**. Omitted rates stay zero:

```json
{"api_name":"metered-model","pricing":{"input":75,"output":300,"cache_read":8,"cache_write_5m":19}}
```

<details>
<summary>Apple Foundation Models notes</summary>

Apple Foundation Models gives sparse metadata. Keep `system` at 8192 context
tokens, `reasoning: true` and `reasoning_configurable: false`. It thinks by
default and offers only `on`, with no configurable `reasoning_effort`. Its
separate `pcc` model has a 32768-token window and low, medium and high effort.
When `fm serve` isn't running, octet skips this optional loopback
`GET /v1/models` without a connection warning. The limits in the example are
model metadata, not global defaults.

</details>

## Model metadata

A pinned models.dev supplement fills in what a provider's model list leaves out:
**display names, pricing, input types and context and output limits**. It covers
only models that built-in discovery actually returns, and it's looked up per
provider. It never supplies tool or structured-output flags or reasoning
controls, and it never replaces a value the endpoint states.

<details>
<summary>How the supplement and its live refresh work</summary>

- **Endpoint assertions stay authoritative.** A live inventory that asserts any
  modality field (including an explicit text-only list, or a false, null or
  malformed assertion) is honored as is, and the supplement isn't consulted for
  modalities at all. Only a sparse inventory that says nothing about input
  modalities may inherit the snapshot's documented `image` and `audio` input.
  Configured and custom metadata and routes keep precedence, and Codex account
  inventory doesn't inherit the supplement. See the [catalog source and pricing
  review](../crates/octet-ai/models/SOURCES.md) for snapshot provenance and the
  difference between retained rich records and the discovery projection.
- **It stays current without an octet release.** Builds never fetch models.dev,
  but an interactive session refreshes it in the background at most every six
  hours and caches the result at `~/.octet/cache/models-dev/metadata.json`.
  `--offline` skips the refresh. Each live record is checked against the
  built-in snapshot first (a price may not become zero or fall more than
  tenfold, for example), and a record that fails keeps the built-in data for
  that model. See [Live
  metadata](../crates/octet-ai/models/SOURCES.md#live-metadata-v082).
- **Why modalities and limits are included.** Several providers publish a sparse
  model list. Direct DeepSeek is the concrete case: its `GET /models` returns
  identifiers only, so a documented vision model such as `deepseek-flash` (V4.1
  Flash) used to register **without** image input (every attachment failed
  closed with "Image input is unsupported") and with the generic 128K/64K
  placeholder instead of its documented 1M context / 384K output, silently
  capping the usable window. Its snapshot record publishes no price, which
  `models-dev-source.json` marks as an unverified pricing provider, so DeepSeek
  spend stays unknown rather than estimated. The snapshot is consulted per
  provider and model, so an entry that declares text-only input keeps that
  decision (`deepseek/deepseek-v4-pro` stays text-only), and a model absent from
  the snapshot gains no capability.
- **What `deepseek-flash` gets.** The supplement supplies the display name
  **DeepSeek V4.1 Flash**, its documented text+image input and its documented 1M
  context / 384K output. These apply only because the endpoint asserts none: a
  model absent from the snapshot, or an endpoint that publishes its own number,
  keeps the generic 128K/64K fallback or the endpoint's value. Structured-output
  support still isn't taken from the snapshot. Its Off/low/high/max reasoning
  and native DeepSeek controls and replay come from the provider-scoped source
  contract, not models.dev, and explicit endpoint reasoning metadata can narrow
  or disable that contract. The endpoint's own `effort` object (`default_level`
  plus `supported_levels`) is decoded when it publishes one, so an unset
  preference uses the declared default level; the pinned fallback states the
  same default. The separate `deepseek-v4` family keeps its declared
  1M/384K limits and Off/high/xhigh reasoning fallback. Direct DeepSeek's
  current peak and off-peak tariff isn't modeled, so pricing stays unknown
  unless explicitly configured, and hard price-dependent ceilings fail closed.

</details>

<a id="endpoint-capability-self-description-unreleased"></a>

## Endpoint capability self-description

An unchanged build can use new models on **already declared Chat or Responses
routes** when the selected endpoint includes an `octet_capabilities` v1 object
in its ordinary model inventory. Built-in OpenAI-compatible, DeepSeek and
OpenRouter discovery and custom-registry startup use the same bounded decoder.
Static-only providers, native Messages, Google, Bedrock and Conversations
routes, Codex and Copilot keep their existing contracts. This creates no new
discovery requests and doesn't bypass a declaration's model filter. Guided
`octet setup` doesn't consume this object yet.

An example entry in `GET /models` (`protocol` uses the canonical Rust API
spelling):

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

<details>
<summary>The rules for this object</summary>

- `open_ai_responses` is the other supported protocol, and it must match the
  existing host-selected route. Version, protocol and positive token limits are
  required, and output can't exceed context. Omitted capability flags are false,
  omitted modalities are text-only, and omitted or null reasoning means no
  reasoning control. Reasoning is an exact effort list, not a guessed range, and
  its optional default must be in the list. The host still picks the
  provider-specific wire encoding.
- Each object is limited to 4096 serialized bytes. Unknown keys or versions,
  malformed flags or options, audio or non-text output, parallel calls without
  tools, and unsupported protocol declarations fail closed. The schema can't
  enable Lite, Ultra or delegation, deferred tools, native budgets or toggles,
  arbitrary profiles, authentication, URLs or transport changes.
- Explicit legacy endpoint assertions (including false, null or unknown) win per
  field. Configured model overrides still win over discovery, except for
  custom-provider limits with discovery enabled, where the live endpoint window
  is authoritative (see the [custom registry](#custom-registry)). No capability
  is borrowed from models.dev. The decoder's provenance names the host-selected
  endpoint, returned model and codec, never an authority or URL claimed by
  response data.
- Built-in raw caches stay URL- and account-isolated and are decoded on use
  without persisting synthesized fields. Custom normalized caches advance to
  version 10, so old sparse results can't hide self-descriptions and old
  configured-wins limit pins can't entomb a live window. These are deterministic
  source contracts, not evidence that any public provider emits the extension
  today.

</details>

## Cold-start feedback

Set `lifecycle_feedback: true` on a custom provider only if its streaming Chat
Completions endpoint implements the optional readiness extension. octet then
sends `x-octet-lifecycle: 1`. The endpoint may answer with that header, comments
such as `: octet-lifecycle: loading; warming model`, or both. Accepted states
are `queued`, `loading` and `ready`. Malformed values and ordinary SSE comments
stay invisible. Endpoints you haven't configured get no header, and ordinary
OpenAI clients ignore it.

<details>
<summary>What the feedback is and isn't</summary>

It's transient, redacted, bounded status. It isn't assistant content, session
history or model context. Plain and print modes write it to stderr, so print
stdout stays response-only. It adds no retries, no replay of accepted POSTs and
no special `503` handling. `startup_timeout_secs` still limits response headers
and feedback can't extend it. Ordinary body idle and deadline limits apply once
a stream starts. Non-streaming requests neither negotiate nor emit feedback. See
the [transport notes](design/octet-ai.md#opt-in-endpoint-lifecycle-feedback).

</details>

## Inference measurements (unreleased)

Every supported conversation codec/transport shares attempt-scoped client
observations, independently of billing and optional native server timing.
[Inference measurements](inference-metrics.md) defines sources, scopes,
unavailability, `/status`, JSONL/NDJSON/RPC events and the route coverage matrix.
Server timing is reported only when a recognized matching terminal count/duration
pair exists; client E2E throughput is never labeled server decode speed. No
live-provider timing accuracy or released availability is implied.

All live conversation routes also use the same usage-calibrated streaming decode
estimator when native timing is absent. Native reporting wins; estimates are
marked `~` and exclude E2E, prefill and completion-tail time. Short/buffered output,
unseparated reasoning and deferred retrieval may remain unavailable. See the
measurement contract for assumptions and qualification limits.

## Reasoning

```sh
octet --reasoning high
octet --reasoning budget=16000
```

`budget=N` works only for compatible models. `/thinking [level]` offers `off`,
`on`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max` or `ultra`, narrowed to
what the selected model supports. A model's own controls (off-only, on and off,
or custom named values) decide the picker and what's sent, not a generic effort
guess. Built-in native discovery may use a declaration-owned token-budget table
only when the whole table fits strictly below the effective output ceiling.
Otherwise the inventory model keeps its limits but advertises no reasoning
control: octet doesn't enlarge the ceiling or invent replacement budgets from
the snapshot. Compatible endpoint choices and defaults can narrow the declared
contract without changing its native codec.

`ultra` needs advertised Ultra/V2 metadata **and** the trusted, enabled, live
`octet-subagents` service. Otherwise it's clamped to the highest ordinary effort
that's safe. Child work uses the extension's `subagent_*` tools and the worker
list in `/extensions`. There's no parallel native root collaboration tool. See
[legacy Pro configuration](configuration.md#compatibility-inputs), [context
budgeting](context.md) and [reasoning
display](terminal.md#reasoning-and-progress).

<a id="defaults-unreleased"></a>

### Defaults

In this checkout, a new CLI session with no reasoning preference uses the
selected model's advertised default, including an advertised Off. Without a
default, a known reasoning contract uses its first supported enabled choice, and
with no usable contract octet's selection stays Off without guessing a reasoning
parameter. This fixes startup overriding model defaults with Off in octet 0.8.0.
An endpoint that publishes its own enabled levels and default (DeepSeek's
`effort: {default_level, supported_levels}` object) is decoded: those levels
become the exact choices, the declared level is the default, and the enabled
levels never remove a declaration-owned Off control. `max` stays selectable
whenever the endpoint declares it, and an explicit selection always wins over
the declared default.

The declared level is only the fallback for a user who has never chosen. The
order is: this run's `--model`/`--reasoning` flag, then the selection the user
saved, then the model's declared default. What is saved is the session's own
record — `EntryValue::Config` written by `append_config_if_changed` and restored
by `persisted_session_config`/`launch_configuration_parts`
(`crates/octet-coding-agent/src/app/bootstrap.rs`) — and the user-level
`~/.octet/config.toml` values that `/model`, `/thinking` and
`/settings default model|reasoning` persist through `cli::persist_model` /
`cli::persist_reasoning`. Resume prefers the session's record over that file and
an explicit `--reasoning` over both; the flag never rewrites the file.
`cargo test --locked -p octet-coding-agent --test reasoning_persistence` covers
each of those restarts.

An explicit CLI, environment or config choice still wins over the model default.
Resume keeps the saved choice (including Off) unless `--reasoning` overrides it,
and existing saved Off values aren't reinterpreted as unset. In the interactive
UI, submit `/thinking on` or another supported level to change and persist that
session's selection, then check `/status`.

A custom `none/default` contract's On leaves the reasoning control absent, so
the server uses its default, and always-on models also get no control parameter.
Unknown metadata and a lack of displayed reasoning don't establish that the
server has disabled thinking. Reasoning can raise latency and token use, so use
`--reasoning off` when the model supports it to opt out. See [reasoning
selection and thinking](provider-thinking.md).

<details>
<summary>OpenRouter reasoning contract</summary>

The OpenRouter route decodes that provider's own per-model `reasoning` object
(`mandatory`, `default_enabled`, `supported_efforts`, `default_effort`) rather
than inferring optionality from the mere presence of a `reasoning_effort`
parameter. Mandatory models offer only their advertised efforts (for GLM 5.3,
`max/high/low`, default `max`), or only `on` if no efforts are published. A
saved unsupported `off` is normalized to an advertised choice, with a
diagnostic. Local summaries use the same contract: Off only when supported,
otherwise the advertised default. A parameter list without an exact contract
offers only the endpoint default, never a guessed `none/minimal/low/medium/high`
range.

For the OpenRouter profile, `off` **omits** the reasoning object instead of
sending `effort: "none"` or `enabled: false`, so the endpoint may still reason
according to its default. Enabled efforts are sent verbatim, and an enabled
boolean-only contract sends `enabled: true`. Other providers keep their existing
explicit-disable behavior. This is a wire-compatibility policy, not a guarantee
that selecting Off disables reasoning on OpenRouter.

</details>

## Protocols and transport

| Protocol | Streaming | Tools | Reasoning | Images | Structured output |
| --- | :---: | :---: | :---: | :---: | :---: |
| OpenAI Responses | Yes | Yes | Yes | Yes | Yes |
| OpenAI Chat Completions | Yes | Yes | Yes | Yes | Yes |
| Anthropic Messages | Yes | Yes | Yes | Yes | Yes |
| Amazon Bedrock Converse | Yes | Yes | Depends on the model (token thinking) | Yes | No |

Capabilities are model-specific and checked before sending: modalities, tools,
structured output, output limits and reasoning. Google uses native
[generateContent](design/octet-ai.md#google-generatecontent), not an OpenAI
translation. Knowing a protocol doesn't imply [native audio
support](media.md#formats-and-limits).

Responses Lite sends `parallel_tool_calls: false` even for models that can run
tools in parallel. Only parallel-safe reads overlap. Shell and file changes stay
one at a time whatever the model batches.

<details>
<summary>OpenAI, Codex and Responses details</summary>

- Direct OpenAI and Codex declare `WebSocketPreferred` with HTTP/SSE fallback
  for ordinary requests. Native steering needs its bidirectional WebSocket
  operation and doesn't replay accepted input through HTTP. Codex also uses
  endpoint-configured zstd request compression, and if compression fails, octet
  sends the valid uncompressed body. These come from the provider
  [declarations](../crates/octet-coding-agent/src/providers/declarations.json),
  not from the Responses codec itself.
- Responses Lite applies to ordinary and native compact requests. It sends the
  Lite header, tool schemas and developer instructions as input items, reasoning
  context across all turns, and no unsupported image-detail hints. Exact shapes:
  the [Lite notes](design/octet-ai.md#responses-lite) and [wire
  fixtures](../crates/octet-ai/src/protocol/openai_responses.rs).
- The Responses `service_tier` request field (`auto`, `default`, `flex`,
  `priority`) is a declared endpoint capability, not a provider identity. The
  codec sends it only when the caller selects one **and** the route's declared
  `runtime.responses_profile` accepts it (`Codex` today, the same gate `/fast`
  uses). Any other profile fails closed with a typed unsupported error instead
  of silently dropping a billing-changing control. Cost settlement and
  conservative request reservations use the declared Codex tier tariff with the
  provider's echoed tier: flex is one-half, and priority is twice the base rate
  (five-halves for exact API model `gpt-5.5`). Unknown tiers, unresolved `auto`
  and unsupported tariffs stay unpriced. The priority uncertainty marker stays
  durable, and tier settlement doesn't clear prior exposure.
- The OpenAI Responses **computer-use tool**
  (`{"type":"computer_use_preview","display_width":…,"display_height":…,"environment":…}`)
  is likewise a declared capability. A caller must select it
  (`ResponsesOptions::with_computer_use`) **and** the route's declared
  `runtime.responses_profile` must accept it (the public Responses profile
  today, with any other profile failing closed with a typed unsupported error).
  octet carries the protocol only: it declares the tool, maps a provider
  `computer_call` to a canonical tool call named `computer_use_preview` with a
  bounded action payload (`click`, `double_click`, `drag`, `keypress`, `move`,
  `screenshot`, `scroll`, `type`, `wait`; anything else fails closed, including
  a missing action), and maps the caller's result back to a
  `computer_call_output` item carrying the one documented `computer_screenshot`
  object. **Nothing in octet executes a computer action.** There's no desktop or
  browser backend behind this codec, and whether any action may run is a
  separate host-policy decision.
- `previous_response_id` is a best-effort, in-process shortcut on a live
  WebSocket, used only when fixed parameters and the prior input and output
  prefix match. It isn't a durable cursor. On resume or a mismatch, octet
  replays everything locally. Native Responses uses persisted, route-affine
  opaque replay.

</details>

<details>
<summary>Connection loss, replay and unknown usage</summary>

The [WebSocket code](../crates/octet-ai/src/responses_ws.rs) and the [recovery
notes](tools.md#recovery-and-security) tell a recognized pre-generation
connection-lifetime rejection (retire the socket, retry safely over HTTP) from
an accepted POST or a body disconnect. By default, ambiguous accepted requests
aren't replayed, even before visible output. The agent has a narrow exception
for host-qualified Codex Responses requests (including Lite) with local function
tools: before assistant commit it may discard provisional output and replace
interrupted inference from durable context. That can duplicate remote generation
and incur unknown charges, and it doesn't replay committed local tool effects or
prove remote cancellation. The agent keeps finite streamed-inference replacement
and HTTP-admission retry budgets separate, without resetting on transport
fallback. They bound attempts, not confirmed accepted generations or charges,
and eligibility and hard ceilings can stop recovery earlier.

Only positively classified pre-send outages enter sustained, cancellable network
waiting. Unknown failed-attempt usage blocks replacement under hard cumulative
cost and token ceilings. Unknown exposure is durably recorded independently of
known usage, and later success, resume or checkout doesn't restore complete
totals. Displayed numeric usage and cost are then a known subtotal, and a fork
starts independent accounting. See the [agent recovery
contract](design/octet-agent.md#in-process-provider-recovery) for eligibility,
budgets and verification limits. Deterministic tests aren't live-provider
recovery qualification.

</details>

## GPT-6 contracts and execution

Direct `gpt-6-astra`, `gpt-6-sol`, `gpt-6.1-sol` and `gpt-6-luna` are declared
on Responses with text and image input, a 1.05M-token context window and 128K
output. Astra accepts `low` through `max` (default low). Sol and Luna also
accept Off/`none` and default to medium. 6.1 Sol defaults to medium but accepts
only `low` through `max` (no `none` or `minimal`), and its tools need the
Responses API. Exact public pricing and its above-272K tier are recorded in [the
catalog sources](../crates/octet-ai/models/SOURCES.md). New public prices aren't
borrowed for Codex Sol, Luna or 6.1 Sol, whose subscription cost stays unknown.

Model and endpoint feature declarations must **both** opt in:

- **Async tools.** Qualified host-parallel observations advertise `async: true`.
  Complete calls are persisted before a bounded background job starts, the model
  can advance while those jobs run, and each result uses its original call ID.
  This isn't speculative execution of streamed arguments. Synchronous calls,
  effectful work, approvals and hard usage ceilings keep their barriers.
  Interrupted pending work isn't blindly rerun after a restart.
- **Native steering.** Public GPT-6 uses `response.steer` on the active socket,
  with durable admission before dispatch and separate accounting and persistence
  for every response segment. Provider acceptance isn't proof that an
  instruction was followed, and ambiguous disconnects aren't automatically
  replayed. Routes or input forms without qualification keep ordinary queued
  steering, and Codex doesn't gain native steering from its model name. Hard
  cumulative ceilings keep the ordinary queue boundary.
- **Thinking changes.** Qualified `/thinking` changes queue for the next
  response boundary without cancelling the root run. The request-level reasoning
  baseline stays fixed, and ordered `configuration_update` items carry ordinary
  effort changes. Off still needs an advertised `none`, and Ultra or
  delegation-mode changes aren't ordinary wire effort updates. Durable replay
  keeps the baseline and the effective selection. Compaction must rebase only
  after a successful summary, and standalone native compact rejects update
  histories without separate authority.

Codex currently qualifies reasoning updates from positive account inventory, not
async tools or native steering. Lite and V2 don't imply either feature. Public
API support and deterministic loopback tests aren't live OAuth inference or
cache-hit qualification. See [reasoning controls](provider-thinking.md) and [the
protocol contract](../crates/octet-ai/docs/responses-controls.md).

Third-party GPT-6 inventory doesn't inherit these capabilities from a name or
snapshot. OpenRouter must advertise its own images, tools and exact reasoning
choices, and its ordinary Chat route doesn't become a Responses control
endpoint. Provider-managed misalignment monitoring is independent of host
approval: `misalignment_policy_violation` stops automatic retry, and already
completed work isn't undone. octet doesn't provision project webhooks or
safety-alert subscriptions automatically.

## Declarative preset metadata

Presets are data, never provider-name branches. The typed preset surface in
`octet_ai::declarations` describes per-model and per-provider fields, and
validation is fail-closed. It's declared plumbing: the OpenAI-compatible codecs
and the streaming client that consume these fields are owned separately, so
preset validation alone doesn't establish codec emission or proxy behavior.

<details>
<summary>The declared fields, credential aliases and proxy resolver</summary>

The typed surface (`ModelPreset`, `RequestOverrides`,
`ProviderCredentialPreset`, `ChatTemplateValue`) covers `samplingParams`,
per-model `headers`, `vllmPriority`, `supportsMaxOutputTokens`,
`thinkingTokenBudgetField`, `chatTemplateArgs` and `chatTemplateKwargs` with
`{ "$var": "thinking.enabled" | "thinking.effort" | "thinking.budget" }`
interpolation, the `string-thinking` format, and credential environment aliases.
Unknown `$var` names, malformed headers, empty identifiers and unbounded retry
or timeout values are rejected.

Credential aliases recognize Anthropic's `ANTHROPIC_AUTH_TOKEN` and
`ANTHROPIC_OAUTH_TOKEN` (which must be sent as `Authorization: Bearer`) ahead of
`ANTHROPIC_API_KEY` (sent as `x-api-key`), and Vertex's `GOOGLE_CLOUD_API_KEY`.
Route presentation currently follows the declaration's static auth presentation,
and selecting bearer or API-key per matched variable isn't implemented.

`octet_ai::declarations::proxy` resolves `HTTP_PROXY`, `HTTPS_PROXY`,
`ALL_PROXY` and `NO_PROXY` for a request target with upstream root-and-subdomain
`NO_PROXY` semantics (exact host, `.domain`, `*.domain`, optional `:port`, lone
`*`), and rejects non-http(s) proxies. The streaming client doesn't call this
resolver.

</details>

<a id="native-mistral-conversations-unreleased-codec"></a>

## Native Mistral Conversations (experimental codec)

The native Conversations codec passes its deterministic request and SSE
fixtures, including rejection of credential-bearing or non-TLS destinations
before credential resolution (literal loopback HTTP is allowed for local
testing). Only native completion settles calls: a missing
`conversation.response.done` reports `MissingFinish`, never a synthesized
tool-call end or successful reply. No POST is replayed. The built-in Mistral
preset above still uses Chat. Conversations discovery, presets and broader
native capabilities are separate work, not implied by these codec repairs.
