# Additive Pi parity delivery ledger

Reference (read-only): `earendil-works/pi` at
`8a7b0c03dfb702663acafb6dc29f8acaa4ffe391` (v0.85.1+72). Verified locally.
This user-selected scope is additive to `BACKLOG.md`, not a replacement for its
roadmap. New parity behavior follows this reference over the older 0.84.4
baseline; historical qualification is not retroactively upgraded.

## Current local 0.8.0 candidate — qualification in progress

Pi parity is **not achieved**. `release-v0.8.0-confirmed` is an older preserved
binary, not qualification of newer source. No installation, publication or SDK
distribution version bump is implied (SDK remains 0.7.6).

Earlier bounded parent receipts under `/tmp/octet-final/` (before current repairs):

- `parity-next-check-10.log`: all-target/all-feature workspace check **exit 0**,
  Cargo 13.20s; this is compilation, not all-feature behavioral qualification.
- `parity-next-coding-lib-02.log`: **1517 passed, 0 failed, 1 ignored**, exit 0.
- `parity-next-agent-tests-03.log` (`--no-fail-fast`): **809 passed, 1 failed,
  2 ignored**, exit 101; includes **153 passing `agent_run` tests**. Sole failure
  is `api_v03_runnable`: published host pin `=0.7.6` versus candidate 0.8.0.
  Private test-staging repair is reported; no subsequent Rust pass is claimed.
- `parity-next-extension-bundle-02.log`: **7 passed**, exit 0, after directory-link/
  command-membership repair; not official release or installation evidence.
- Renderer receipts remain **277** default / **270** no-syntax tests passed.
  AI **480** is historical phase-1 evidence, **not phase-2 qualification**.

Parent reports all **1632 source hashes stable** at the recheck snapshot in
`parity-next-recheck-source.sha256`. Subsequent kernel F3/F4/F6, AI sampling F7 /
enforceable cap F5 / RequestOverrides / Azure / RPC turn-cost, extension bus SDK
lifecycle F2, F8 export privacy and typed-media HTML repairs are **not qualified
by those receipts**. No all-green current-worktree or full-workspace-test claim.

**Current receipts over the frozen `freeze-f5.sha256` snapshot** (1635 files):
workspace all-target/all-feature `f5-check` **exit 0**; coding library
`f5-coding-lib` **1522 passed, 0 failed, 1 ignored**; agent library
`f5-agent-lib` **572 passed, 0 failed, 1 ignored**; `agent_run` `f5-agent-run`
**156 passed, 0 failed** (includes the new context-rejection recovery); every AI
target `f5-ai` **exit 0**; `parity_cli` **18 passed** (includes the sparse-provider
image/limit regression through the real CLI); `eval_harness` **10 passed**;
`migration_import` + `pi_install` + `provider_contract` **exit 0**; local release
build `f5-release` **exit 0** (`target/release/octet` reports 0.8.0). The earlier
whole-suite receipts remain valid for their snapshots: `f2-check`, `f2-coding`
**1703 passed across 30 targets**, `f2-agent` **819 passed**, `f2-ai`
**500 passed**.
Supporting non-Cargo receipts: Python SDK **109 passed**; Pi bridge **88 run /
85 passed / 3 real-runtime skips**; scripts **58 passed**; TypeScript conformance
**47 fixtures**; generator `--check` clean. Renderer receipts: **277 passed**
(`parity-next-renderer-04`).

Preserved failures (repaired, not deleted): coding library **1503/9/1** and
**1519/2/1**; the intermittent `eval_harness` failure now traced to a non-blocking
accept in its loopback fixture (fixed, four consecutive green runs, and green in
`f5-eval`); one load-induced
`update::progress::tests::actual_updater_progress_pty_and_plain_streams` timeout
(2-second subprocess budget) that passed in the immediately following full run; agent library **564 passed** with `agent_run` **148/4**, then
**559/9/1** with `agent_run` **154/1**; AI `provider_parity` **26/2**; Pi bridge
**36 failures**; two in-progress compile failures (`E0583`, `E0063`); one
`eval_harness` failure under concurrent load that passed four later standalone
runs. Concrete causes and per-file repairs are enumerated in
[VERIFICATION.md](VERIFICATION.md#current-candidate--qualification-in-progress).
A fresh whole coding-agent/agent target rerun (`parity-next-coding-all-07`,
`parity-next-agent-all-08`) was launched over `parity-next-wave5-source.sha256`;
read those logs rather than this summary.

**Local candidate caveat.** Exact host pins mean the four tracked official
bundles still declare `requires_octet = "=0.7.6"` (only `octet-subagents` was
repinned to the local `=0.8.0`), so this local 0.8.0 candidate does not load
already-published bundles. Tests that need current-source behavior stage private
copies carrying the current version while asserting tracked manifests keep their
published pins. A real 0.8.0 release would re-release every bundle; nothing here
claims that occurred.

**F8 privacy blocker — repaired in source, then rerun.** The common
`session_commands` JSON/HTML export boundary now retains only explicit
`public:true` values in real `Entry.metadata.extension_metadata`, even with
`--include-secrets`, and does not recursively strip similarly named tool data.
Authored coverage: two formats × two secret-flag modes, explicit/default-private
metadata on an abandoned branch, source preservation/reopen and private output
permissions. Rich HTML now traverses only typed User/Assistant/ToolResult Media;
arbitrary `Image`/`Audio` metadata stays data. Those cases execute inside the
1600-test coding library receipt above; the audit's F8 remains a correct finding
about the pre-repair source.

## Historical frozen review

**Integration status (post-crash `final-audit`, base `df5a7e80`; historical frozen
receipt).** Pi parity was **not achieved**. `Verified`
means the stated behavior has a recorded passing test, not that every suite is
green; new receipts are in the fresh review. Unrefreshed `Landed` / `Verified`
rows retain their bounded historical evidence. `Implemented` means inspected
source with its behavioral verification pending; `Partial` names missing behavior.
A failed unrelated test does not erase a specific passing regression. In particular:

- `Unverified` marks a row whose implementation may exist but has no per-row
  recorded observation in this pass. It is deliberately not upgraded on the
  strength of a passing neighbouring test. Final all-target/all-feature workspace
  check and the recorded library/integration/doc-test receipts are green. Earlier
  ENOSPC, failed and interrupted runs remain historical—not successful commands.
  Final behavioral evidence is sharded, not a monolithic workspace-test pass.
- Rows marked `Partial`, `Blocked`, `Pending` or `In progress` name the exact
  missing primitive below or in the fresh final-review section; they are not
  silently absent. Old detail documents can under- or over-claim wiring.

[`VERIFICATION.md`, fresh final review](VERIFICATION.md#final-review--final-audit-post-crash)
is the independent adversarial pass over these
claims (VERIFIED / CONTRADICTED / UNVERIFIED) and is the honest basis for the PR
body; where it disagrees with a detail document, prefer it. Generated
provider/model artifacts must be regenerated by re-running their script; no
upstream TypeScript is vendored.

## Detail documents

Each detail owner records exact upstream anchors, evidence and remaining gates. A
`Pending` row above is owned by one of these pages; a landed row links the page
section that owns it.

- [Providers](providers.md) — provider declarations, catalogs and request surfaces.
- [Codec depth](codecs.md) — per-codec codec/streaming depth.
- [Tools](tools.md) — built-in tool parity.
- [TUI](editor.md) — terminal UI and editor parity.
- [Telemetry](telemetry.md) — vendor-neutral telemetry surface.
- [CLI](cli.md) — CLI and output-mode parity.
- [Repo tooling](repo-tooling.md) — rows `6.1`–`6.4` (docs, maintainer artifacts, catalog diff, changelog tooling).
- [Extensions](extensions.md) — extension-surface parity: extension API capabilities, MCP transports and computer use.

## Non-negotiable exclusions

No persisted project-trust changes; no host-brokered OAuth/credential-policy
changes; no clipboard image capture; no rg/fd auto-download; no chord, CBOR or
Unix-socket architecture work. In particular `/settings` cannot implement a
persisted trust default under this brief. OAuth flow depth must preserve the
existing host policy rather than bypassing it.

## Coverage inventory

**Historical 88-row snapshot:** required scope is unchanged; states below retain
the frozen review, not current-source status. Apply the current source overlay
above before interpreting old “missing”, “Unverified” or “Landed” wording.
The detail documents own exact upstream/source/test anchors and per-subitem
outcomes. Grouped options below remain individually required, not alternatives.

| ID | Required behavior | Detail owner | State |
| --- | --- | --- | --- |
| 1a.1 | baseten, qwen-token-plan, qwen-token-plan-cn, qwen-token-plan-individual, zai-coding-cn declarations/catalog/tested #252 rows | providers | Partial; declarations/discovery present, exact pinned static catalogs missing |
| 1a.2 | radius + pi-messages codec + dynamic discovery | codec depth | Blocked; PiMessages codec, declaration and dynamic discovery missing |
| 1b.1 | Per-request apiKey, headers, env, fetch, onPayload, onResponse, timeoutMs, maxRetries, maxRetryDelayMs, metadata, transformHeaders | providers | Partial; request-local `RequestOverrides` (headers, env, timeout, bounded retries) and Azure destination/API-version options are consumed on the wire by `stream_with_overrides`/`complete_with_overrides` with `provider_parity` coverage; runtime provider hooks (`onPayload`/`onResponse`/`transformHeaders` callbacks) and per-request `apiKey`/`fetch` remain absent by policy or design |
| 1b.2 | Model/request samplingParams and model headers | providers | Partial; model preset sampling params and headers now merge into the wire with an explicit allowlist (legacy `functions`/`function_call`, hosted web search and stored-prompt keys are refused, and caller `stop` wins), request temperature precedence is covered, and header secrecy/signing order is tested; per-model thinking/priority emission for the remaining providers is still outstanding |
| 1b.3 | HTTP_PROXY / HTTPS_PROXY / ALL_PROXY / NO_PROXY root and subdomain semantics | providers | Partial; pure resolver exists, transport integration missing |
| 1b.4 | Conditional catalog etag / If-None-Match / checkedAt | providers | Verified; five conditional-inventory loopback regressions passed |
| 1b.5 | vllmPriority, supportsMaxOutputTokens, thinkingTokenBudgetField, chatTemplateArgs/Kwargs, $var interpolation, string thinking | providers | Partial; interpolation/types exist, codec emission missing |
| 1b.6 | ANTHROPIC_AUTH_TOKEN, ANTHROPIC_OAUTH_TOKEN, GOOGLE_CLOUD_API_KEY aliases | providers | Partial; bearer aliases landed, vertex ADC/API-key kind pending |
| 1c.1 | Strict JSON-schema and grammar/Lark/regex custom tools across five codec families; strict-prefer defaults | codec depth | Partial; explicit declarations emit; custom-call decode/replay, strict default/model compatibility missing |
| 1c.2 | Deferred additional_tools/tool-search and Anthropic tool_reference emit paths, or removal of unsupported claims | codec depth | Landed; unsupported claim removed |
| 1c.3 | Anthropic OAuth/fine-grained/interleaved betas, mid-conversation effort, caller-beta merge | codec depth | Partial |
| 1c.4 | Anthropic refusal fallbacks, blocks and fallback-model pricing | codec depth | Partial |
| 1c.5 | Bedrock profile ARN regions/application profiles/web identity/bearer token | codec depth | Partial; bearer/web identity present, model/profile ARN region wiring missing |
| 1c.6 | Codex per-request sse/websocket/websocket-cached/auto, connect deadline and debug stats | codec depth | Partial; host retry/envelope regressions pass; per-request modes/stats still missing |
| 1c.7 | Azure deployment map and per-call deployment/base URL/resource/API-version overrides | codec depth | Partial; `ResponsesRuntimeProfile::Azure` plus per-call deployment/deployment-map/base-URL/API-version options select the destination on the wire (percent-encoded path and query preserved) under `provider_parity`; live Azure acceptance is not claimed |
| 1c.8 | Mistral Conversations reasoning_effort | codec depth | Blocked; native reasoning_effort/prompt_mode profile and emission missing |
| 1c.9 | xAI Responses + encrypted reasoning replay | codec depth | Partial; encrypted-reasoning replay exists and the current-reference xAI route is now `openai-responses` in the in-memory *current* fixture view only, with the historical 0.84.4 inventory deliberately preserved as `openai_chat` and asserted as an intentional difference; live xAI acceptance is not claimed |
| 1c.10 | rawStopReason, responseModel, providerThinkingLevel, diagnostics and per-tool-result usage | codec depth | Partial; diagnostics exist, raw/model/thinking/per-tool usage fields missing |
| 1d.1 | apiKey.check/resolve, oauth.login/refresh/logout, AuthCheck, minOAuthValidityMs, isSubscription | auth depth | Partial; host check/resolve seam missing; OAuth policy expansion excluded |
| 1d.2 | Unified tagged provider credential store, list/modify/delete, provider-scoped env | auth depth | Excluded; alternate credential-store policy not authorized |
| 1d.3 | Anthropic OAuth, xAI device, OpenRouter PKCE/manual redirect, Kimi device; RFC8628/PKCE/callback helpers | auth depth | Excluded; new brokered OAuth flows not authorized |
| 1e.1 | Faux provider pending/ready/failed/cancelled deferred handles + deferred stop reason | streaming | Pending |
| 1e.2 | Assistant message frame encoder/reducer and durable partial republish | streaming | Verified; partial republish and secure bounded journal regressions pass; not atomic frontend delivery |
| 1e.3 | Image generation API, OpenRouter adapter, generated image-model catalog + generator | streaming | Pending |
| 2a.1 | Alternate screen, autowrap, fixed viewport, final document to main screen | TUI | Unverified |
| 2a.2 | Constrained layout root/VStack/HStack/ScrollView sizing/visibility/nested scrolling/last-frame hit-testing | TUI | Unverified |
| 2a.3 | Alt-wheel ×5 and unused-delta chain/contain | TUI | Partial; Alt-wheel scrolls fifteen rows instead of three and the existing wheel-delta chaining is retained, but the explicit unused-delta chain/contain subitem is not separately demonstrated |
| 2a.4 | Half-page, line, top/home, bottom/end viewport bindings | TUI | Partial; half-page, line and top/bottom bindings dispatch in the running shell, while Home/End reuse editor semantics instead of a dedicated viewport binding |
| 2a.5 | Clickable jump to latest | TUI | Partial; the existing scroll indicator is a clickable return-to-live affordance when history is scrolled, but the alternate-screen/fixed-viewport behavior of 2a.1 is still absent, so this is not the upstream mechanism |
| 2b.1 | Namespaced configurable JSON keybindings, conflicts, platform defaults | editor | Verified for the executed cases; the running shell loads `~/.octet/keybindings.json` at real entry with bounded descriptor-bound reads, explicit unbinding, reload/conflict diagnostics and native/WSL defaults, and the product dispatch tests pass. Reserved safety keys stay deliberately non-rebindable |
| 2b.2 | Undo/redo with coalescing | editor | Verified for the executed cases; the product dispatches configured undo/redo, mapped cursor motion ends a typing run (without which the shell coalesced edits incorrectly), and the dispatch tests pass |
| 2b.3 | Kill-ring/yank/yank-pop | editor | Verified for the executed cases; kill/yank/yank-pop are dispatched by the running shell and no longer survive a motion that should end the yank run |
| 2b.4 | Word/line deletion and forward/backward jumps | editor | Verified for the executed cases; word/line deletion and word jumps are dispatched with real editor text/selection assertions |
| 2b.5 | OSC133 A/B/C zones and prompt jumps | editor | Verified for the executed cases; a lazy generation-keyed semantic prompt index derived from trusted block boundaries drives prompt/page jumps and clamps after growth or reflow, adding no ANSI to copy/no-color output and forging no markers from model or user escapes |
| 2b.6 | Focus reporting and focus-out interaction reset | editor | Landed |
| 2c.1 | Cached transcript search panel, match/current styling, prev/next click, Escape | TUI | Verified for the executed cases; cached search with current/match styling, previous/next/Escape/click, content/width invalidation and **input precedence** (query events and bracketed paste intercepted before clipboard admission and extension shortcuts) are covered by seven transcript-navigation tests plus two idle/active owner tests |
| 2c.2 | Hidden/auto/always themed transient scrollbar | TUI | Verified for the executed cases; the three visibility modes render themed transient scrollbars with focus/reset handling |
| 2c.3 | LaTeX rendering | TUI | Landed; oracle-swept (1061 cases, 0 divergences), 407 goldens |
| 2c.4 | Mermaid box-drawing diagrams | TUI | Landed; bounded self-captured subset, unsupported syntax fails closed |
| 2c.5 | Component mouse events/MouseRegion/capture/focus/hover/click selection/link hit-test/right paste | TUI | Unverified |
| 2c.6 | Native text clipboard read with existing write fallback | TUI | Implemented; clipboard helper regressions pass; native-platform qualification pending |
| 2d.1 | /settings defaults/theme/transport/images/editor padding | TUI | Unverified |
| 2d.2 | /scoped-models all/clear/provider toggle/reorder/persistence/cycling | TUI | Unverified |
| 2d.3 | /hotkeys | TUI | Verified for the executed cases; `/hotkeys` is a built-in command rendered from the resolved binding set |
| 2d.4 | /debug rendered lines and message JSON | TUI | Unverified |
| 2d.5 | /copy last message | TUI | Verified for the executed cases; `/copy` returns the last clean assistant message through `copy_last_assistant()` |
| 2d.6 | /compact custom instructions | TUI | Verified for the executed cases; `/compact` accepts bounded local instructions (16 KiB, control-checked) through `summarize_with_retry` host accounting, and native Responses compaction explicitly refuses them instead of dropping them |
| 2d.7 | Tree bookmark/timestamp toggles and five filter modes | TUI | Withdrawn by maintainer decision; the `/tree` and `/checkout` slash commands were deleted on 2026-09-16, so the upstream tree surface has no local consumer. The durable connector tree remains readable through `octet sessions inspect`. |
| 2d.8 | Resume threaded/relevance sorting and full-transcript search | TUI | Verified for the executed cases; Resume gained Relevance and Threaded ordering (iterative parent-before-child, orphans/cycles stay visible, no cross-store ID linking) and the transcript search above |
| 2d.9 | Model cycling forward/back and selector hotkeys | TUI | Partial; forward/back cycling follows the App-supplied scope order, de-duplicates while preserving it and advances from the last queued target while active, with selector bindings and a `--models` glob scope; the `--reasoning` suffix ordering and full ordered-scope parity remain |
| 2d.10 | /session file/id/message/token/cost detail | TUI | Pending |
| 2d.11 | !command / !!command context exclusion | TUI | Pending |
| 3.1 | Explicit callback-based vendor-neutral TelemetryContext/TelemetrySpan, no global/exporter | telemetry | Landed |
| 3.2 | NOOP and InMemory implementations | telemetry | Landed |
| 3.3 | Serializable typed span/schema definitions | telemetry | Landed |
| 3.4 | Span assertion harness | telemetry | Landed |
| 3.5 | Provider/stream/tool/turn/compaction/summary/delegation span boundaries | telemetry | Verified; all seven span boundaries covered by passing agent/delegation regressions |
| 3.6 | Tool and summary usage in totals, cache-hit rate, distinct cacheWrite1h; preserve uncertainty | telemetry | Verified; tool/summary usage and distinct cacheWrite1h are covered by passing agent tests, and unpriced-or-uncertain provenance now drives summaries, RPC projections and TUI status (`has_unpriced_usage()` OR `has_uncertain_usage()`), not scalar subtotals alone |
| 4.1 | ls directories/dotfiles/limit | tools | Withdrawn by maintainer decision; behaviour served by ripgrep-backed `search` |
| 4.2 | find glob/gitignore/limit | tools | Withdrawn by maintainer decision; behaviour served by ripgrep-backed `search` |
| 4.3 | Default grep: ignoreCase/context/limit/hidden | tools | Withdrawn by maintainer decision; behaviour served by ripgrep-backed `search` |
| 4.4 | Bash spilled output path | tools | Landed |
| 4.5 | Bash session identity/provider/model/reasoning env + commandPrefix | tools | Landed |
| 4.6 | Opt-in PowerShell, Windows CI evidence | tools | Partial; agent-only opt-in, coding-product allowlist missing; Windows evidence gated |
| 4.7 | Interval durable partial bash output checkpoints | tools | Verified for the session-backed sink; `enable_session_partial_output_checkpoints("bash", BASH_CHECKPOINT_INTERVAL)` is opt-in, live checkpoint publication and settlement/late-handle fencing are covered by passing agent tests, and historical checkpoint bytes remain private append-only transcript data rather than securely erased records |
| 4.8 | Adaptive preview coalescing, bounded interval/rate/single trailing timer | tools | Verified; live agent pacer coalescing/settlement regression passed |
| 4.9 | Original-file nonoverlapping multi-edit + legacy normalization | tools | Landed |
| 4.10 | Unanimous finalized-result batch termination | tools | Verified; unanimous durable-result batch and lone-request regressions passed |
| 4.11 | Durable invocation memos through replay until outcome known | tools | Verified for the executed cases; session-backed memos/partial state, capability injection at wave admission, a settled-invocation tombstone that refuses reopening a completed identity, and stale/foreign handle fencing are covered by passing agent tests. A crash before ordered result placement remains an unresolved call (no exactly-once outcome claim) |
| 4.12 | Deferred provider suspend/resume/handles/poll permits | tools | Partial; decision core only, provider fetch and durable suspended-run lifecycle missing |
| 4.13 | Tool promptSnippet/promptGuidelines | tools | Verified; opt-in agent prompt consumer passed; coding-product default unchanged |
| 4.14 | Summarization retry distinct from compaction failure | tools | Verified for the executed cases; `summarize_with_retry`/`summarize_branch_with_retry` own reservation, retries and durable usage while the caller commits once, accepted-but-unsettled auxiliary results retain known usage or durable uncertainty, and ordinary-route retry/commit-once tests pass. Manual-summary retry *progress* is still not surfaced to the UI (callbacks are dropped) |
| 5.1 | --mode json session-event JSONL | CLI | Verified |
| 5.2 | --list-models optional search | CLI | Verified |
| 5.3 | --session-id and --name | CLI | Verified |
| 5.4 | --no-session ephemeral with accounting intact | CLI | Verified; all-session RPC, append-retry and torn-write recovery regressions pass |
| 5.5 | @file/media expansion and multiple sequential positional prompts | CLI | Verified |
| 5.6 | Piped stdin in every mode | CLI | Verified |
| 5.7 | --models glob cycling constraints | CLI | Partial; the glob scope is verified and now feeds ordered forward/back cycling through the App-supplied scope order; requested ordered `--reasoning` suffix handling remains |
| 5.8 | Single-file safe theme/Markdown/highlight/ANSI/media HTML export + goldens | CLI | Partial; safe JSON/media HTML projection, not Markdown/highlight/ANSI rendering |
| 5.9 | Incremental session/entry search + change notification | CLI | Verified |
| 5.10 | Catalog publish min-client/required-provider/count/checksum/immutable-path gates | CLI | Verified |
| 5.11 | Isolated model-backed eval harness/artifacts/pass/latency/cost deltas | CLI | Partial; isolated scripted-loopback harness, real model-backed evaluation missing |
| 6.1 | Docs for settings/session format/keybindings/compaction/templates/providers/packages/shell aliases/terminal/tmux/termux/Windows | repo tooling | Landed (12 topic pages; 239 relative links, 0 unresolved; receipt in [repo-tooling.md](repo-tooling.md#verification-status)) |
| 6.2 | Maintainer prompts and release/add-provider/interactive-testing skills; AGENTS conventions | repo tooling | Landed (4 prompts, 3 skills, tracked conventions mirror `docs/maintainers/conventions.md`; root `AGENTS.md` stays local-only) |
| 6.3 | HEAD/worktree catalog diff including effective reasoning levels | repo tooling | Landed |
| 6.4 | CHANGELOG release extraction and link repair | repo tooling | Landed |
| 6.5 | Deterministic git-archive source artifact pinned by version/ref | repo tooling | Landed |

Tracked route/API/theme/chrome/queue/media/lifecycle/import work is referenced
rather than duplicated. No remote publication or GitHub issue operation is
performed by this local delivery.

## Current delivery limits

**Historical subsection, retained with its original heading.** The bus-only-SDK,
API-0.2-only packager and missing-consumer statements below describe the frozen
snapshot. The current source overlay and current-candidate verification supersede
them; unresolved implementation and independent acceptance gates remain open.

- API 0.3 theme selection is **unavailable**, now omitted from the product offer
  with a passing real-process canonical-refusal regression. A host handler is
  still missing; the protocol defect is fixed, not the feature implemented.
  The event bus remains an SDK kernel, not a host `bus/*` service.
- `open-all` now refuses product pane execution in source pending atomic host
  writer claim/settlement. The extension owner reports **88 passing tests**,
  including repeated fresh-launchable requests with zero pane effects. This
  verifies refusal, not pane handover; the functionality remains Partial.
- Importer entrypoint Git modes are corrected to `100755`. The separate release
  packager still rejects API 0.3 (`scripts/package-octet-extension-release.sh:119`);
  executable modes and runtime/API tests do not qualify release packaging.
- A primitive's former “consumer pending” label must not survive when the actual
  consumer now exists (1e.2, 3.5, 4.7, 4.8, 4.10, 4.11, 4.13, 4.14, 2b.*, 2c.1,
  2c.2, 2d.3, 2d.5, 2d.6). Conversely, session-backed state is still not
  crash-durable for **4.12** (deferred provider suspend/poll has no transport or
  durable suspended-run lifecycle), and 4.14's manual-summary retry progress is
  not surfaced to the UI. No pane handover, exact-once effect or physical
  terminal claim follows from any of these receipts.
- Deliberate exclusions above are not missing implementation tasks. Windows,
  native automation, live remote/provider, terminal and signed/public release
  evidence remain separate gates; fixtures do not close them. See the final
  review for the precise primitive/evidence split and unresolved findings.
