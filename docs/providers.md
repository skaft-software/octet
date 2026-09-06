# Providers and models

[Documentation](README.md) · [Configuration](configuration.md) · [Media](media.md)

```sh
export ANTHROPIC_API_KEY='...'
octet --safe-mode --model claude-sonnet-4-6
```

Use `/model [id]` to select a model and `/status` to inspect its route and
capabilities. Live discovery is used where a provider exposes it. `--offline`
skips optional discovery, **not inference traffic**.

> Draft qualification: these are supplied 0.7.0 source contracts, not live
> endpoint verification. Provider/thinking work outside this snapshot and
> contradictory context-window documentation require reconciliation before publication.

## Cloud setup

Set the credential variables for the chosen row, then run `octet --model ID`.
Do not put credentials into prompts or repository configuration.

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

Bedrock accepts an `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` pair with an
optional session token, the selected `AWS_PROFILE`, or ECS/EC2 instance metadata.
Model availability depends on account and region. Quote IDs containing shell
metacharacters, for example
`octet --model 'bedrock/anthropic.claude-3-7-sonnet-20250219-v1:0'`.

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
and [compatibility reference](pi-provider-compatibility.md) describe route-specific
coverage; a preset name is not a promise of every provider API.

## Codex subscription login

```sh
octet --login codex
octet --model gpt-5.6
```

This uses hosted device login instead of a manually managed API key. A successful
account-scoped live inventory is authoritative. octet does not infer Ultra,
collaboration, Responses Lite, or model availability from a name or subscription
plan. Missing or unusable metadata falls back conservatively. If live inventory
omits Astra, no Codex Astra route is injected; when present, select
`codex/gpt-6-astra` independently of a direct OpenAI preset.

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
0.2 implementation boundary. Catalog installation is [publication-gated](installation.md#optional-packages).

GitHub Copilot is **not** a CLI login/configuration preset. A Rust embedding host
can own its device flow, OAuth storage/exchange/refresh, and vetted inference
origin through the [credential-safe SDK seam](sdk.md#host-owned-github-copilot).
Neither standalone octet nor NDJSON `octet-host` accepts Copilot credentials.

## Local and custom endpoints

With no configured model, interactive setup offers **LM Studio** or an
**OpenAI-compatible endpoint**. Choose one endpoint, an optional credential
source, a discovered/manual model ID, and review before saving. No localhost or
network scan occurs. Compatible servers include llama.cpp, vLLM, SGLang, LM Studio,
and compatible gateways.

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

`ultra` requires advertised Ultra/V2 metadata **and** the trusted, enabled,
live `octet-subagents` service. Otherwise it is clamped to the highest ordinary
safe effort. Child work uses extension `subagent_*` tools and `/subagents`;
there is no parallel native root collaboration tool surface. See
[legacy Pro configuration](configuration.md#compatibility-inputs),
[context budgeting](context.md), and [reasoning display](terminal.md#reasoning-and-progress).

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

Direct OpenAI defaults to HTTP/SSE Responses. Codex uses `WebSocketPreferred`
with HTTP/SSE fallback and endpoint-configured zstd HTTP request compression;
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

`previous_response_id` is a best-effort process-local live-WebSocket optimization
only when fixed parameters and the prior input/output prefix match. It is not
a durable cursor: resume or mismatch uses full local replay; native Responses
uses persisted route-affine opaque replay. The
[WebSocket implementation](../crates/octet-ai/src/responses_ws.rs) and
[recovery boundary](tools.md#recovery-and-security) distinguish a recognized
pre-generation connection-lifetime rejection (socket retirement, safe HTTP retry)
from an accepted POST or body disconnect. Body disconnects remain terminal even
before visible output. Deterministic regressions are not live-provider recovery
qualification.

## Astra source limits

Direct `gpt-6-astra` is declared on Responses with text/image input, a 1.05M-token
context window, 128K output, and `low` through `max` effort. Inputs above 272K
use the long-context price tier. Baseline 0.7.0 source supports selection,
text/images, reasoning, and ordinary/parallel tool calls. It does **not** implement
native async tools (`async: true` and pending-call lifecycle), steering an active
Responses WebSocket response, or coding-loop reasoning changes through
`configuration_update` with verified cache preservation. Execution-time input is
queued for a later model-turn boundary; `parallel_tool_calls` is not native async.
These limits also apply to Codex Astra. Public API support does not prove OAuth
endpoint support; additional capabilities require fresh account-scoped metadata
or verified endpoint behavior.
