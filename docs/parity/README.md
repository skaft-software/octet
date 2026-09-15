# Additive Pi parity delivery ledger

Reference (read-only): `earendil-works/pi` at
`8a7b0c03dfb702663acafb6dc29f8acaa4ffe391` (v0.85.1+72). Verified locally.
This user-selected scope is additive to `BACKLOG.md`, not a replacement for its
roadmap. New parity behavior follows this reference over the older 0.84.4
baseline; historical qualification is not retroactively upgraded.

**Integration in progress.** Pending below is not a compatibility claim. Each
item must finish with behavioral tests, docs and a CHANGELOG entry, or a named
missing primitive/release blocker. Generated provider/model artifacts must be
regenerated; no upstream TypeScript is vendored. A source/type/load-only check
never qualifies behavior.

## Non-negotiable exclusions

No persisted project-trust changes; no host-brokered OAuth/credential-policy
changes; no clipboard image capture; no rg/fd auto-download; no chord, CBOR or
Unix-socket architecture work. In particular `/settings` cannot implement a
persisted trust default under this brief. OAuth flow depth must preserve the
existing host policy rather than bypassing it.

## Coverage inventory

The detail documents own exact upstream/source/test anchors and per-subitem
outcomes. Grouped options below remain individually required, not alternatives.

| ID | Required behavior | Detail owner | State |
| --- | --- | --- | --- |
| 1a.1 | baseten, qwen-token-plan, qwen-token-plan-cn, qwen-token-plan-individual, zai-coding-cn declarations/catalog/tested #252 rows | providers | Pending |
| 1a.2 | radius + pi-messages codec + dynamic discovery | codec depth | Pending; new primitive is release-blocking |
| 1b.1 | Per-request apiKey, headers, env, fetch, onPayload, onResponse, timeoutMs, maxRetries, maxRetryDelayMs, metadata, transformHeaders | providers | Pending |
| 1b.2 | Model/request samplingParams and model headers | providers | Pending |
| 1b.3 | HTTP_PROXY / HTTPS_PROXY / ALL_PROXY / NO_PROXY root and subdomain semantics | providers | Pending |
| 1b.4 | Conditional catalog etag / If-None-Match / checkedAt | providers | Pending |
| 1b.5 | vllmPriority, supportsMaxOutputTokens, thinkingTokenBudgetField, chatTemplateArgs/Kwargs, $var interpolation, string thinking | providers | Pending |
| 1b.6 | ANTHROPIC_AUTH_TOKEN, ANTHROPIC_OAUTH_TOKEN, GOOGLE_CLOUD_API_KEY aliases | providers | Pending |
| 1c.1 | Strict JSON-schema and grammar/Lark/regex custom tools across five codec families; strict-prefer defaults | codec depth | Pending |
| 1c.2 | Deferred additional_tools/tool-search and Anthropic tool_reference emit paths, or removal of unsupported claims | codec depth | Pending |
| 1c.3 | Anthropic OAuth/fine-grained/interleaved betas, mid-conversation effort, caller-beta merge | codec depth | Pending |
| 1c.4 | Anthropic refusal fallbacks, blocks and fallback-model pricing | codec depth | Pending |
| 1c.5 | Bedrock profile ARN regions/application profiles/web identity/bearer token | codec depth | Pending |
| 1c.6 | Codex per-request sse/websocket/websocket-cached/auto, connect deadline and debug stats | codec depth | Pending |
| 1c.7 | Azure deployment map and per-call deployment/base URL/resource/API-version overrides | codec depth | Pending |
| 1c.8 | Mistral Conversations reasoning_effort | codec depth | Pending |
| 1c.9 | xAI Responses + encrypted reasoning replay | codec depth | Pending |
| 1c.10 | rawStopReason, responseModel, providerThinkingLevel, diagnostics and per-tool-result usage | codec depth | Pending |
| 1d.1 | apiKey.check/resolve, oauth.login/refresh/logout, AuthCheck, minOAuthValidityMs, isSubscription | auth depth | Pending; broker policy excluded |
| 1d.2 | Unified tagged provider credential store, list/modify/delete, provider-scoped env | auth depth | Pending; broker policy excluded |
| 1d.3 | Anthropic OAuth, xAI device, OpenRouter PKCE/manual redirect, Kimi device; RFC8628/PKCE/callback helpers | auth depth | Pending; broker policy excluded |
| 1e.1 | Faux provider pending/ready/failed/cancelled deferred handles + deferred stop reason | streaming | Pending |
| 1e.2 | Assistant message frame encoder/reducer and durable partial republish | streaming | Pending |
| 1e.3 | Image generation API, OpenRouter adapter, generated image-model catalog + generator | streaming | Pending |
| 2a.1 | Alternate screen, autowrap, fixed viewport, final document to main screen | TUI | Pending |
| 2a.2 | Constrained layout root/VStack/HStack/ScrollView sizing/visibility/nested scrolling/last-frame hit-testing | TUI | Pending |
| 2a.3 | Alt-wheel ×5 and unused-delta chain/contain | TUI | Pending |
| 2a.4 | Half-page, line, top/home, bottom/end viewport bindings | TUI | Pending |
| 2a.5 | Clickable jump to latest | TUI | Pending |
| 2b.1 | Namespaced configurable JSON keybindings, conflicts, platform defaults | editor | Pending |
| 2b.2 | Undo/redo with coalescing | editor | Pending |
| 2b.3 | Kill-ring/yank/yank-pop | editor | Pending |
| 2b.4 | Word/line deletion and forward/backward jumps | editor | Pending |
| 2b.5 | OSC133 A/B/C zones and prompt jumps | editor | Pending |
| 2b.6 | Focus reporting and focus-out interaction reset | editor | Pending |
| 2c.1 | Cached transcript search panel, match/current styling, prev/next click, Escape | TUI | Pending |
| 2c.2 | Hidden/auto/always themed transient scrollbar | TUI | Pending |
| 2c.3 | LaTeX rendering | TUI | Pending |
| 2c.4 | Mermaid box-drawing diagrams | TUI | Pending |
| 2c.5 | Component mouse events/MouseRegion/capture/focus/hover/click selection/link hit-test/right paste | TUI | Pending |
| 2c.6 | Native text clipboard read with existing write fallback | TUI | Pending; images excluded |
| 2d.1 | /settings defaults/theme/transport/images/editor padding | TUI | Pending; persisted trust default forbidden |
| 2d.2 | /scoped-models all/clear/provider toggle/reorder/persistence/cycling | TUI | Pending |
| 2d.3 | /hotkeys | TUI | Pending |
| 2d.4 | /debug rendered lines and message JSON | TUI | Pending |
| 2d.5 | /copy last message | TUI | Pending |
| 2d.6 | /compact custom instructions | TUI | Pending |
| 2d.7 | Tree bookmark/timestamp toggles and five filter modes | TUI | Pending |
| 2d.8 | Resume threaded/relevance sorting and full-transcript search | TUI | Pending |
| 2d.9 | Model cycling forward/back and selector hotkeys | TUI | Pending |
| 2d.10 | /session file/id/message/token/cost detail | TUI | Pending |
| 2d.11 | !command / !!command context exclusion | TUI | Pending |
| 3.1 | Explicit callback-based vendor-neutral TelemetryContext/TelemetrySpan, no global/exporter | telemetry | Pending |
| 3.2 | NOOP and InMemory implementations | telemetry | Pending |
| 3.3 | Serializable typed span/schema definitions | telemetry | Pending |
| 3.4 | Span assertion harness | telemetry | Pending |
| 3.5 | Provider/stream/tool/turn/compaction/summary/delegation span boundaries | telemetry | Pending |
| 3.6 | Tool and summary usage in totals, cache-hit rate, distinct cacheWrite1h; preserve uncertainty | telemetry | Pending |
| 4.1 | ls directories/dotfiles/limit | tools | Pending |
| 4.2 | find glob/gitignore/limit | tools | Pending |
| 4.3 | Default grep: ignoreCase/context/limit/hidden | tools | Pending |
| 4.4 | Bash spilled output path | tools | Pending |
| 4.5 | Bash session identity/provider/model/reasoning env + commandPrefix | tools | Pending |
| 4.6 | Opt-in PowerShell, Windows CI evidence | tools | Pending |
| 4.7 | Interval durable partial bash output checkpoints | tools | Pending |
| 4.8 | Adaptive preview coalescing, bounded interval/rate/single trailing timer | tools | Pending |
| 4.9 | Original-file nonoverlapping multi-edit + legacy normalization | tools | Pending |
| 4.10 | Unanimous finalized-result batch termination | tools | Pending |
| 4.11 | Durable invocation memos through replay until outcome known | tools | Pending |
| 4.12 | Deferred provider suspend/resume/handles/poll permits | tools | Pending |
| 4.13 | Tool promptSnippet/promptGuidelines | tools | Pending |
| 4.14 | Summarization retry distinct from compaction failure | tools | Pending |
| 5.1 | --mode json session-event JSONL | CLI | Pending |
| 5.2 | --list-models optional search | CLI | Pending |
| 5.3 | --session-id and --name | CLI | Pending |
| 5.4 | --no-session ephemeral with accounting intact | CLI | Pending |
| 5.5 | @file/media expansion and multiple sequential positional prompts | CLI | Pending |
| 5.6 | Piped stdin in every mode | CLI | Pending |
| 5.7 | --models glob cycling constraints | CLI | Pending |
| 5.8 | Single-file safe theme/Markdown/highlight/ANSI/media HTML export + goldens | CLI | Pending |
| 5.9 | Incremental session/entry search + change notification | CLI | Pending |
| 5.10 | Catalog publish min-client/required-provider/count/checksum/immutable-path gates | CLI | Pending |
| 5.11 | Isolated model-backed eval harness/artifacts/pass/latency/cost deltas | CLI | Pending |
| 6.1 | Docs for settings/session format/keybindings/compaction/templates/providers/packages/shell aliases/terminal/tmux/termux/Windows | repo tooling | Pending |
| 6.2 | Maintainer prompts and release/add-provider/interactive-testing skills; AGENTS conventions | repo tooling | Pending |
| 6.3 | HEAD/worktree catalog diff including effective reasoning levels | repo tooling | Pending |
| 6.4 | CHANGELOG release extraction and link repair | repo tooling | Pending |
| 6.5 | Deterministic git-archive source artifact pinned by version/ref | repo tooling | Pending |

Tracked route/API/theme/chrome/queue/media/lifecycle/import work is referenced
rather than duplicated. No remote publication or GitHub issue operation is
performed by this local delivery.
