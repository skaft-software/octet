# OpenRouter Batch API

octet exposes OpenRouter's asynchronous Batch API as an explicit command path. It
is intended for independent requests that can wait in the provider queue; the
interactive agent continues to use synchronous streaming because tool calls and
conversation turns need an immediate response.

## Setup

Set the existing OpenRouter credential and select an OpenRouter model:

```sh
export OPENROUTER_API_KEY='...'
octet batch submit \
  --model openrouter/anthropic/claude-sonnet-4.6 \
  --input requests.json
```

The model must be present in octet's OpenRouter catalog. Normal startup discovers
that catalog from OpenRouter; `--offline` skips network discovery and first uses a
matching local inventory cache. For an explicitly supplied
`openrouter/<provider>/<model>` slug with no cache, the batch command uses
conservative local metadata and leaves availability validation to OpenRouter.

## Input

`--input` accepts a regular local JSON file or `-` for stdin. It may be either a
bare array or an object with a `requests` array. The object form may also include
`endpoint` and `model`; if present, those values must agree with the selected
command options/model.

```json
{
  "requests": [
    {
      "custom_id": "item-001",
      "body": {
        "messages": [
          {"role": "user", "content": "Independent task"}
        ]
      }
    }
  ]
}
```

`custom_id` values must be non-empty and unique. Each `body` must be a JSON
object and uses the native body shape for the selected endpoint. The selected
model's OpenRouter API slug is sent once at the batch level; a body-level
`model` is optional but, if present, must match it. The endpoint defaults to the
protocol advertised by the selected model and can be overridden with:

- `/v1/chat/completions`
- `/v1/responses`
- `/v1/messages`
- `/v1/embeddings`

OpenRouter's Batch API currently accepts text-only requests. octet does not
translate canonical conversations, run tools, or perform follow-up turns inside
a batch. Use the ordinary agent path for those workflows.

## Lifecycle

Submission prints the provider's batch object and id:

```sh
octet batch submit --model openrouter/openai/gpt-4o --input requests.json
```

Retrieve once, or poll until OpenRouter reports a terminal status:

```sh
octet batch get batch_123 --model openrouter/openai/gpt-4o
octet batch get batch_123 --model openrouter/openai/gpt-4o --wait --poll-seconds 30
```

`batch status` and `batch retrieve` are aliases for `batch get`. List jobs with
cursor, status, and creation-time filters:

```sh
octet batch list \
  --model openrouter/openai/gpt-4o \
  --status completed \
  --limit 50
```

OpenRouter processes the batch asynchronously within a 24-hour completion
window. Results are returned inline by OpenRouter after completion and remain
available for 30 days; there is no separate results-download command. Failed,
expired, or cancelled batches have no results array. octet does not automatically
retry submission or retrieval; this avoids silently creating duplicate
asynchronous jobs. HTTP failures retain request-id, retry-after, and retryability
metadata in the shared `octet-ai` error taxonomy.

OpenRouter documents the current completion window, pricing exceptions, BYOK
routing, and retention period at <https://openrouter.ai/docs/batch-quickstart>.

## Library API

`octet-ai` exposes:

- `OpenRouterBatchRequest` and `OpenRouterBatchRequestItem` for validated
  submission envelopes;
- `AiClient::submit_openrouter_batch`;
- `AiClient::get_openrouter_batch`; and
- `AiClient::list_openrouter_batches` with cursor/status filters.

These methods reuse octet endpoint authentication, redirect policy, timeout
bounds, response-size bounds, and diagnostic redaction. Batch operations are
provider-specific and do not alter the provider-independent interactive request
or stream types.
