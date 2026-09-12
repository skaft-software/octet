# octet SDK and native host

Rust applications embed the public `octet-agent` and `octet-ai` crates. The
`octet_sdk` library in `octet-coding-agent` contains the product runtime shared
by `octet` and `octet-host`.

Other languages launch `octet-host` and exchange UTF-8 JSON objects over
stdin/stdout, keeping provider and agent behavior in Rust without an unstable
Rust FFI ABI. Stdout is protocol-only; logs and diagnostics go to stderr.

**Native-host protocol `1` is not extension API `0.3`.** It is a separate
application embedding interface. It reports extension discovery diagnostics but
never starts executable extensions. For extension authoring, see
[the API `0.3` guide](extensions.md).

## Handshake

The example reports this checkout's **0.7.6 source version**. For version-matched
published native assets, see [installation](installation.md) and the
[release record](releases/v0.7.6.md).

Send `hello` and validate the response before accepting work, including when the
application uses a configured host path:

```json
{"protocol_version":1,"request_id":"probe-1","command":"hello"}
```

```json
{"protocol_version":1,"request_id":"probe-1","seq":1,"type":"hello","data":{"sdk_version":"0.7.6","protocol_version":1,"max_frame_bytes":1048576,"max_concurrent_runs":1,"commands":["hello","models","run","shutdown"],"features":{"streaming":true,"persistent_sessions":true,"seed_history":true,"typed_media_input":true,"typed_image_input":true,"typed_audio_input":true,"prompt_display_text":true,"inline_models":true,"tools":true,"skills":true,"extensions":true,"process_group_abort":true,"in_band_abort":false}}}
```

Reject a protocol mismatch, unknown request ID, run/session ID mismatch, or
sequence gap. The `extensions` feature does not grant process-start authority.

## Install

From a checkout, install both binaries:

```console
cargo install --locked --path crates/octet-coding-agent --bins
```

This is a source installation. Cargo installs embed
text documentation in the binaries. On first use, octet materializes it under
`${CARGO_HOME:-$HOME/.cargo}/share/octet`; the managed copy is refreshed when the
Cargo-channel `octet update` installs a newer release.

## Protocol invariants

- Protocol version: `1`.
- Encoding: UTF-8 NDJSON, exactly one object per line.
- Maximum request or event frame: 1 MiB, **including the terminating newline**.
- Requests are serial; `hello` reports `max_concurrent_runs: 1`.
- Every request has `protocol_version` and a caller-generated `request_id`.
- Every event echoes `request_id` and has per-request `seq` starting at `1` and
  increasing by exactly one.
- Run events also carry `run_id` and echo `session_id` when supplied.
- A run terminates with one `final_result` or `protocol_error`. `hello`, `models`,
  and `shutdown` each terminate with the same-named event.
- Malformed, oversized, or unknown request fields produce a bounded
  `protocol_error`; the reader discards the rest of that line before accepting
  another request. Strict request objects prevent misspelled authority or
  capability fields from silently falling back to defaults. Negotiate advertised
  request features through `hello`; tolerate additive host event fields.
- Oversized outbound values are replaced with terminal bounded `protocol_error`,
  never written as oversized frames.
- EOF exits cleanly. Successful shutdown responses flush before exit.
- Drain stderr separately and bound retained diagnostics. Never parse stderr as
  protocol.

Request/run/session IDs are at most 128 bytes and contain only ASCII letters,
digits, `-`, `_`, `.`, and `:`. They are identifiers, not paths.

## Model inventory

`models` returns the resolved catalog:

```json
{"protocol_version":1,"request_id":"models-1","command":"models","offline":true}
```

`offline: true` suppresses live discovery while constructing the catalog.
Each model reports `input_modalities` (including implied `text`) plus separate
legacy `vision` and additive `audio` booleans. These values are route-effective:
audio is advertised only when both model and selected protocol support native
audio input. Offline mode is not an OS network sandbox and does not stop a later
run calling its provider.

### Host-owned GitHub Copilot

GitHub Copilot is embedding-only Rust integration, absent from `octet --login`,
environment/configuration setup, and NDJSON `octet-host`: those surfaces cannot
safely own the host's GitHub OAuth state. Standalone catalogs never advertise
Copilot models.

An embedding app implements `octet_sdk::provider::CopilotHost`, owns device-flow/
OAuth state and durable credential storage, and constructs `CopilotProvider`
with explicit `CopilotEndpoint`. The host can display bounded `CopilotDeviceLogin`
from `begin_device_login`, poll until `Authorized`, then `register_models`.
Registration checks host availability, exchanges a short-lived inference session,
obtains credential-free authenticated inventory, validates all models, and adds
routes atomically. Any login/exchange/discovery/validation/catalog-collision
failure leaves Copilot out of the picker.

The host supplies a vetted HTTPS inference origin root; literal loopback HTTP
is allowed only for deterministic tests, and path/query/userinfo are rejected.
It supplies short-lived `CopilotSession` and explicit `Protocol` metadata per
model. `OpenAiChat` selects Chat Completions and `OpenAiResponses` Responses;
other protocols are rejected and model names never choose a codec. The resolver
exchanges when no session exists and refreshes within its safety skew. Session
credentials/dynamic headers stay in memory behind `Auth::Dynamic`, marked
sensitive on requests and redacted from diagnostics, never in provider definitions,
catalog metadata, or persistence. Hosts must also exclude credentials from model
IDs and display labels.

The seam does not implement GitHub's live OAuth endpoints, Enterprise endpoint
policy, or an interactive CLI flow. These are host-owned; use Rust embedding
only when the host can implement and test that policy.

## Rust-owned recovery limits

Rust embedding hosts can call
`Agent::set_max_network_wait(Option<Duration>)` before starting a run. This
bounds elapsed recovery after the first positively identified pre-send outage
in a logical turn, including subsequent request opening. `None` is the default
(no outage-duration ceiling); zero disables outage waiting. A minimum retry
delay beyond the remaining allowance stops recovery rather than retrying early.
This is not a whole-job timeout and does not extend caller/child limits or
provider body deadlines. The same setting is passed to auxiliary compaction
and terminal-gate recovery and inherited by child agents. Historical integration
checks and remaining limits are recorded in [v0.7.4 recovery qualification](qualification/v0.7.4-recovery.md).

This is a Rust host setter, not a new NDJSON run field, CLI flag, or persisted
configuration setting. NDJSON applications retain process-group cancellation.

## Run requests

Required run-specific fields are `run_id`, `workspace`, `model`, and `prompt`,
alongside `protocol_version`, `request_id`, and `command: "run"`:

```json
{"protocol_version":1,"request_id":"req-1","command":"run","run_id":"run-1","session_id":"customer-42","workspace":"/srv/workspace","session_dir":"/srv/state/octet-sessions","model":"gpt-5.6","prompt":"Summarize MEMORY.md","tools":["read"],"allow_file_mutation":false}
```

Documented optional fields:

| Field | Behavior |
| --- | --- |
| `working_dir` | Invocation directory; must resolve inside `workspace`. |
| `session_id` | Stable ID creating `<session_dir>/<id>.jsonl`. |
| `session_dir` | Application-owned root; defaults to `<workspace>/.octet/sessions`. |
| `resume_session` | Existing regular JSONL file confined to `session_dir`; final symlinks are rejected. |
| `system_prompt` | Application-owned system context, up to 512 KiB. |
| `prompt_display_text` | Exact caller-visible text when `prompt` has model-only composition. May be empty; capped at 256 KiB; never reaches the model. Send only when `hello.features.prompt_display_text` is true. |
| `history` | Seed `user`/`assistant` messages for a new session only; at most 256 messages and 2 MiB. |
| `tools` | Explicit registration allowlist. `[]` disables tools; omission uses the default surface. Registration never bypasses effect admission. |
| `allow_file_mutation` | False removes edit/write/process/shell authority. True retains those gates but does not relax Controlled effect policy. |
| `allow_external_paths` | Allows caller-supplied session/media paths outside the workspace. Model-controlled file tools remain workspace-only under fixed Controlled policy. |
| `context_files` | Enables/disables normal trusted workspace context files. |
| `reasoning` | Reasoning level accepted by the selected model. |
| `max_turns` | Run turn limit; omission has no ceiling, matching the interactive default. |
| `max_cost_microdollars` | Exact integer run-cost ceiling. |
| `media` | Ordered `{"type":"image","path":"…"}` or `{"type":"audio","path":"…"}` inputs; at most 12 items: eight images and four audio clips. |
| `image_paths` | Legacy image-only input; cannot combine with `media`. |
| `prompt_paths`, `skill_paths` | Explicit resource roots. |
| `extension_paths`, `enabled_extensions`, `trusted_extensions` | Discovery/trust configuration. Protocol v1 reports diagnostics but never starts extension processes. |
| `offline` | Suppresses bootstrap live discovery, not provider traffic. |

Prompts cap at 512 KiB. Display text caps at 256 KiB and rejects control
characters except newline/tab; it is durable presentation metadata only, while
`prompt` remains the exact replayable model input.

PNG/JPEG/GIF/WebP images cap at 5 MiB each and 20 MiB total. WAV/MP3/FLAC/Opus/AAC/
PCM16 inputs are recognized, with 20 MiB per clip and 40 MiB total. The selected
route must natively support the exact format; currently OpenAI Chat accepts WAV
and MP3. Media uses descriptor-bound, symlink-resistant reads, is retained as
typed session input in request order, and is sent from original bytes. Media
bytes never cross the NDJSON frame.

Two visual references around an audio reference:

```json
{"protocol_version":1,"request_id":"req-media","command":"run","run_id":"run-media","workspace":"/srv/workspace","model":"gpt-audio-1.5","prompt":"Compare the references and describe the music.","media":[{"type":"image","path":"/srv/workspace/moodboard/one.png"},{"type":"audio","path":"/srv/workspace/music/theme.wav"},{"type":"image","path":"/srv/workspace/moodboard/two.jpg"}]}
```

### Inline providers

Define one route without changing global config:

```json
{"protocol_version":1,"request_id":"req-local","command":"run","run_id":"run-local","workspace":"/srv/workspace","model":"local-model","provider":"local","base_url":"http://127.0.0.1:1234/v1","api_key":"application-owned-secret","custom_headers":{"x-tenant":"example"},"provider_mode":"openai-compatible","context_window_tokens":32768,"max_output_tokens":4096,"input_modalities":["image"],"supports_reasoning":false,"prompt":"Reply with OK","tools":[]}
```

The inline example's route fields are `provider`, `base_url`, `api_key`,
`custom_headers`, `provider_mode`, `context_window_tokens`, `max_output_tokens`,
`input_modalities`, and `supports_reasoning`. The legacy `vision: true` field
remains equivalent to declaring `image`. Keep application-owned credentials out
of application logs and diagnostics.

Supported `provider_mode` values are exactly `openai-compatible` (Chat
Completions), `openai-responses`, and `anthropic-messages`. `input_modalities`
is an explicit capability assertion and may contain `image` and `audio`. A local
or OpenAI-compatible endpoint alone does not establish audio support: advertise
it only when the exact model/route is known to accept native audio through
OpenAI Chat.

Inline base URLs must be absolute HTTP(S), have a host, and omit userinfo/query/
fragment. Route/model IDs are SHA-256-derived and isolated from the built-in
catalog. Custom headers cap at 64 entries and 64 KiB total; hop-by-hop/routing
headers are rejected. Anthropic keys use `x-api-key`; other modes use Bearer auth.

Without `base_url`, `model` uses normal catalog/credential resolvers. If no Codex
credential exists on first use, the host can copy a valid third-party Codex CLI
credential from `~/.codex/auth.json`. It does not import earlier Hamr/Ygg stores.
The imported value is written to `~/.octet/credentials/codex.json` with owner-only
permissions under the cross-process refresh lock. Source credentials are never
modified or deleted.

## Event lifecycle

A successful run normally emits:

1. `accepted` with resolved model, native session path, registered tools, and a
   secret-safe `effective_tool_policy` snapshot of capability limits/source layers,
   excluding raw workspace and shell paths;
2. `started`;
3. zero or more streaming events;
4. `settled`;
5. exactly one `final_result`.

Streaming events include:

- `model_delta`, `output_media`;
- opt-in `provider_lifecycle` readiness telemetry;
- `provider_retry`, `provider_waiting_for_network`, `provider_operation_retry`,
  `provider_usage_uncertain`, `candidate_rejected`;
- `tool_start`, `tool_policy`, `tool_progress`, `tool_finish`;
- `model_step` usage/cost accounting;
- `steering_delivered`, `follow_up_delivered`;
- `compaction_start`, `compaction_finish`;
- `extension_notification`.

`tool_policy` carries secret-safe allowed/denied metadata and stable denial
codes, never command content or raw shell paths. `accepted.data.effective_tool_policy`
and `tool_policy.data.decision.policy` share one schema. `effect_policy`,
`workspace_confinement`, `allow_edit`, `allow_write`, `allow_process`,
`allow_shell`, `shell_path`, `bash_timeout_ms`, `max_output_bytes`, and
`allow_remote_read` each have `{ "value": ..., "source": ... }` with source
`default`, `config`, `environment`, `cli`, or `host_request`.
`shell_path.value` contains only `{ "selection": "configured" | "system_bash" |
"path_bash" | "sh_fallback" | "unavailable" }`, never a path, digest, or cross-run
identifier. Decisions contain optional effect, `allowed`, `authorization` when
allowed, and stable `denial_code` when denied. Allowed evidence emits only after
all host hooks and reservation commit gates finish.

`provider_lifecycle.data` has `state` `queued`/`loading`/`ready` and nullable
bounded `detail`. It emits only for explicitly opted-in configured endpoints,
and is advisory telemetry, not model output or durable content.

`provider_retry.data` carries `attempt`, `max_attempts`, `delay_ms`, and sanitized
`error`. Discard all provisional output/media from the failed attempt, including
reasoning already closed for presentation; do not discard independent activity
or previously committed assistant/tool results. A replacement is not a new run.

The additive `provider_waiting_for_network.data` carries `attempt`, `delay_ms`,
and sanitized `error`, with **no** `max_attempts`: eligible pre-send waiting has
no finite count limit and does not consume the finite inference-replacement
budget. It keeps the run live and is not `settled` or `final_result`. Cancellation
and caller-owned job limits still apply. These event additions do not bump
native-host protocol `1`. The session schema version is unchanged, but the
additive uncertainty record evolves its record contract. CLI RPC uses the same
event type
with `delayMs`/`errorMessage`; finite retries use `auto_retry_start` with
`maxAttempts`, rather than the native-host field casing. Plain/print diagnostics
go to stderr; print stdout remains response-only. See [historical recovery qualification](qualification/v0.7.4-recovery.md).

Auxiliary recovery has a separate core `AgentEvent::ProviderOperationRetry`:
`operation` is `local_compaction`, `native_compaction`, or `terminal_gate`;
`attempt` is one-based; `max_attempts: Option<usize>` is absent as a count limit
for pre-send waiting; `delay` and sanitized `error` describe that operation.
It does **not** invalidate main-answer output or settle the run. Its
`provider_operation_retry` consumer wire uses native-host `operation`, `attempt`,
`max_attempts`, `delay_ms`, and `error`; CLI RPC uses `operation`, `attempt`,
`maxAttempts`, `delayMs`, and `errorMessage`. The nullable maximum is JSON `null`
for pre-send waiting. These fields are additive, not an additional API-version
negotiation.

The unit core event `AgentEvent::ProviderUsageUncertain` maps to native-host
`{"type":"provider_usage_uncertain","data":{}}` (plus ordinary envelope fields)
and CLI RPC `{"type":"provider_usage_uncertain"}`. It neither discards assistant
output nor settles the run. Treat it as sticky session state: later success
does not clear it, and a run on an uncertain resumed session emits it again.
All subsequent numeric usage/cost fields, including cumulative accounting, are
**known subtotals**, not complete totals. Plain/print warn on stderr; interactive
footer/telemetry labels the uncertainty rather than presenting exact totals.

Rust hosts can inspect `Session::has_uncertain_usage()` and
`usage_uncertainty_records()` even when no run is active.
`Session::record_usage_uncertainty(endpoint, model, operation)` durably appends
`{"type":"usage_uncertainty","record":{"endpoint":"codex","model":"openai/gpt-5.4","operation":"assistant_turn"}}`.
It records no fictional tokens or cost, changes neither conversation head nor
known subtotal, and must succeed before replacing the failed attempt. Uncertainty
survives resume, checkout, and compaction; a fork starts independent accounting.
CLI RPC `get_state` and session-statistics snapshots expose additive
`usageUncertain: bool`, including idle/resumed sessions. Active state keeps the
flag sticky when the live event arrives. Statistics sum the independent durable
usage ledger rather than only the active conversation branch; when the flag is
true, numeric tokens/cost are known subtotals. Native-host protocol `1` has no
separate idle session-inspection command; its resumed runs emit the live warning.
Historical qualification applies only to its recorded source.

`final_result.data` contains `status`, `output`, `error`, `filesChanged`,
`toolCalls`, `steps`, and `sessionFile`. Status is `completed`, `blocked`, or
`error`. Failures before/during valid run requests use error `final_result`;
malformed protocol requests use `protocol_error`.

## Headless safety and cancellation

`octet-host` never waits for interactive input. It always uses Controlled effect
policy: pure/workspace-read calls may run; workspace mutation requires approval
that the headless host denies. Ambient host/process, network, delegation,
extension, and unknown effects fail closed. Core confirmations are denied and
typed input cancelled. Controlled prevents executable-extension startup itself;
protocol v1 exposes no unsafe-host opt-in.

There is **no in-band abort command**. Launch each host in its own process group
and terminate the whole group on timeout/caller cancellation. `hello` explicitly
reports `process_group_abort: true`, `in_band_abort: false`. On Unix the host
coordinates `HUP`, `INT`, `QUIT`, and `TERM`: abort active work, give registered
shell process groups bounded cleanup time, force-kill survivors, exit with
`128 + signal`. Registration also reaches shell children that made process
groups outside the host's group.

## Resource and session ownership

The application chooses `workspace`/`session_dir`. The host loads deterministic
resource layers (`~/.octet`, trusted workspace `.octet`, then explicit roots)
and persists native append-only JSONL sessions. Keep application-domain memory
in the application's store and inject retrieved context through `system_prompt`;
use octet sessions for model/tool continuity.
