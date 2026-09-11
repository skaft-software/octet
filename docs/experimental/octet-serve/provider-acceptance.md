# Configured-provider acceptance

Maintainer reference for provider routes and acceptance procedures in the
octet 0.7.5 source. For usage, see the [Serve guide](README.md). Optional
live-provider/native-host audio checks are **NOT RUN** in this source review.
Graphical media, recovery, and capture work remains separately tracked. The
[exact GitHub release](https://github.com/skaft-software/octet/releases/tag/v0.7.5)
records publication verification and any separately approved live acceptance;
source contracts are not live-provider or model-capability qualification.

The retained [v0.4.0 record](#release-record) is historical Ygg evidence, not a
pass or waiver of current release gates.

## Acceptance boundaries

Serve uses the coding agent's provider stack rather than a web-specific client.
The documented workflow separates:

1. required deterministic, credential-free conformance testing on pull requests;
2. optional, separately approved credentialed checks against live providers.

Pull-request CI must not inherit developer or repository provider credentials.
Credentialed results must not upload raw provider traffic, prompts, or logs.
Live credentials and paid API calls are not release prerequisites. Unexecuted
live checks are recorded as **NOT RUN**, never as passing. Deterministic CI,
security, artifact signing and public-install verification remain required.
Nothing on this page authorizes a live run.

The live procedure below builds `octet-host` and tests **native-host protocol 1**,
not extension API 0.3 or the graphical Serve transport. In particular, native
audio acceptance does not establish web attachment support: production Serve
supports images and bounded prompt documents, not audio. The bundled extension
runtime examples are not qualified API 0.3 authoring examples.

<a id="supported-provider-matrix"></a>

## Acceptance route matrix

These are representative routes for the procedures below, not a complete
provider inventory or model-capability qualification. See the
[provider guide](../../providers.md) for additional declared routes, including
Google, Bedrock, Azure, Mistral, and Cloudflare, and their configuration limits.
The protected workflow selects Responses, Messages, Chat, and one native-audio
route; it does not exercise every declared provider.

| Route | Providers | Credential source | Deterministic coverage | Optional live representative |
| --- | --- | --- | --- | --- |
| OpenAI Responses | OpenAI API models | `OPENAI_API_KEY` | `octet-ai` protocol/client tests | OpenAI |
| OpenAI Responses with subscription auth | Codex models | `octet --login codex` or first-use import into octet's owner-only store | protocol, refresh, migration, and redaction tests | Codex when this route changes |
| Anthropic Messages | Anthropic, MiniMax | `ANTHROPIC_API_KEY`, `MINIMAX_API_KEY` | `octet-ai` protocol/client tests | Anthropic |
| OpenAI Chat | DeepSeek, OpenRouter, Groq, Cerebras, xAI, Together AI, Fireworks AI, NVIDIA, Hugging Face, Moonshot AI, Xiaomi, OpenCode Zen | provider variable listed below | `octet-ai` protocol/client tests plus full Serve process acceptance through a local endpoint | OpenRouter or another affected preset |
| Custom OpenAI-compatible Chat | User-defined local or remote endpoint | `none`, `bearer_env`, or the `api_key_env` shorthand in `~/.octet/credentials/custom.json` | full Serve process acceptance through a disposable loopback fixture | One user-configured endpoint when custom routing changes |

The built-in OpenAI Chat credential variables are:

| Provider | Environment variable |
| --- | --- |
| DeepSeek | `DEEPSEEK_API_KEY` |
| OpenRouter | `OPENROUTER_API_KEY` |
| Groq | `GROQ_API_KEY` |
| Cerebras | `CEREBRAS_API_KEY` |
| xAI | `XAI_API_KEY` |
| Together AI | `TOGETHER_API_KEY` |
| Fireworks AI | `FIREWORKS_API_KEY` |
| NVIDIA | `NVIDIA_API_KEY` |
| Hugging Face | `HF_TOKEN` |
| Moonshot AI | `MOONSHOT_API_KEY` |
| Xiaomi | `XIAOMI_API_KEY` |
| OpenCode Zen | `OPENCODE_API_KEY` |

A custom provider's environment-variable name is user-selected in its
`bearer_env` auth entry or `api_key_env` shorthand. API keys must not be placed
directly in Git-tracked configuration. A provider being listed here means the
snapshot documents its wire route; individual model capabilities still depend
on the provider's model metadata.

## Deterministic CI gate

`apps/web/tests/live-host.spec.ts` is documented to launch the real Serve-capable
`octet` binary with a temporary owner-only `HOME`, workspace, credential registry,
and session directory. Its provider is an in-process loopback OpenAI-compatible
server. The child environment is an allowlist containing a fake fixture token,
so ambient `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, and other live credentials cannot
enter the test process.

The test's acceptance coverage is:

- bearer authentication and provider-qualified model selection;
- streamed text;
- a streamed `read` tool call and tool-result replay on the next request;
- retries after fixture `429` throttling and `408` timeout responses, including
  a `429` sequence that exhausts automatic retries;
- explicit `/compact`, durable checkpointing, process restart, and resume;
- cancellation of an in-flight stream; and
- bounded provider/phase failure diagnostics visible after retry exhaustion,
  omitting the provider body, fixture token, prompt canaries, request IDs, and
  provider error codes, and never projecting model-only failed-turn context as
  an assistant response.

The documented local command sequence is:

```sh
cargo build -p octet-coding-agent --bin octet --features serve --locked
cd apps/web
npm ci
npm run test:e2e:live
```

This is the only configured-provider test allowed in ordinary CI. It must stay
loopback-only and credential-free. No current run is reported here.

## Optional credentialed acceptance

The snapshot describes a protected `Stable provider acceptance` workflow. Both
stable release workflows expose `require_provider_acceptance`, defaulting to
`false`. Packaging then does not read provider secrets or require an acceptance
run; this is the credential-free release policy for octet 0.7.5. Setting it to
`true` explicitly opts that workflow run into fail-closed exact-SHA and
protected-approval enforcement.

For a separately authorized run, the `stable-release-provider-acceptance`
environment requires reviewers and these spend-limited secrets:

- `LIVE_OPENAI_API_KEY`
- `LIVE_ANTHROPIC_API_KEY`
- `LIVE_OPENAI_CHAT_BASE_URL` and `LIVE_OPENAI_CHAT_API_KEY`
- `LIVE_AUDIO_BASE_URL` and `LIVE_AUDIO_API_KEY`

The `.github/workflows/provider-acceptance.yml` dispatch contract requires both
workflow ref and `source_sha` to be the exact 40-character candidate commit, with
reviewed model and provider IDs as inputs. Pull requests and forks are excluded.

The workflow builds `octet-host` from that checkout and invokes
`scripts/provider-acceptance.py` in isolated owner-only workspaces. Its criteria
are:

1. OpenAI Responses, Anthropic Messages, and OpenAI-compatible Chat each stream
   text, call the read-only `read` tool, and return a disposable file canary.
2. The exact production `provider:model` selected for native audio consumes an
   integrity-pinned spoken-code WAV attachment and correctly transcribes the
   code, which is not present in the model prompt.
3. Each non-audio route proves the protocol-v1 controlled, host-request policy
   snapshot; registers only `read`; and emits one matching, secret-safe policy
   decision before each registered call finishes. The required successful canary
   decision is `workspace_read` authorized by `policy`; an audio route with no
   tools is the only policy-evidence exemption.
4. Every route obeys the bounded host protocol lifecycle, sequence, scope, and
   terminal-event contract.

Provider stderr is discarded, the child receives an allowlisted environment,
and raw protocol/provider traffic is never uploaded. Workflow logs and the job
summary contain only route labels, provider/model IDs, candidate SHA, and
sanitized pass/fail status. Retry, cancellation, compaction, persistence, and
invalid-credential redaction remain deterministic CI responsibilities; the live
check must not intentionally create billable throttling or network disruption.

When `require_provider_acceptance` is `true`, both stable release workflows query
GitHub's immutable Actions history and fail closed unless the exact source commit
has a successful `workflow_dispatch` run with a recorded approval for
`stable-release-provider-acceptance`. A local run, a run for another SHA, or an
unapproved successful run is not release evidence. When the input is `false`,
the workflow records **NOT RUN** (optional) in its job summary and continues
packaging. This does not claim that any live route or model was tested.

## Manual Serve acceptance

The inherited real-provider Serve checklist has not been run. It is optional,
not a release prerequisite. A separately approved assessment must record
evidence for:

- fresh and restored sessions, including concurrent independent sessions;
- real prompts, streaming, tool activity, and context/compaction accounting;
- image and supported document attachments;
- steer, follow-up, edit, retry, fork, stop, and reconnect;
- terminal reopen and host shutdown;
- archive, trash, restore, and guarded permanent deletion;
- branch checkout and source, diff, and output reopening after host restart;
- review and search.

Fixtures and native-host checks cannot substitute for this graphical journey.
See [web criteria](web-acceptance.md) and the
[Project](https://github.com/orgs/skaft-software/projects/5) for work tracking.

## Release record

Live-provider checks were waived for `v0.4.0`; deterministic configured-provider
coverage remained required and passed. The original record stated that the
optional protected workflow could run later without changing published binaries.
This historical statement does not authorize a run or waive current gates.

| Candidate | Gate | Provider/model | UTC date | Result | Reviewer |
| --- | --- | --- | --- | --- | --- |
| `v0.4.0` pre-release validation | Deterministic Serve configured-provider matrix | `custom/e2e/e2e-model` | 2026-08-10 | PASS | Local release review |
| `v0.4.0` release SHA | OpenAI Responses live representative | Not selected | — | WAIVED — no release credential | Project decision |
| `v0.4.0` release SHA | Anthropic Messages live representative | Not selected | — | WAIVED — no release credential | Project decision |
| `v0.4.0` release SHA | OpenAI Chat live representative | Not selected | — | WAIVED — no release credential | Project decision |
| `v0.4.0` release SHA | Native audio production route | Not selected | — | WAIVED — no release credential | Project decision |
