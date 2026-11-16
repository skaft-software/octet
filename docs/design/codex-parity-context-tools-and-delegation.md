# Codex parity: context, tools, delegation, computer use and MCP

Status: **investigation + proposal**. Nothing here is implemented.

Provenance: octet at `b04d33850`, Codex at `81b995a6c` (2026-11-14), read
directly from both working trees. Every `path:line` below was verified against
those commits.

## Why this exists

Subagents, computer use, code mode and MCP each work in octet, but they do not
compound the way they do in Codex. The reason is one shared root cause, not four
separate gaps: **octet's model-visible tool array is large and unstable, and
every provider caches on the prompt prefix.** Fixing that lifts all four areas
at once.

The rest of this document establishes that claim, then lists six changes in
priority order with acceptance criteria.

## The thesis

Provider prompt caching keys on a byte-stable prefix: instructions, then the
**tool schema array**, then messages. Tool schemas sit at the very front. Any
mutation to that array invalidates the cached prefix for the *entire*
conversation, and the next request pays full input price for a 200K-token
session.

Four routine octet actions mutate that array mid-session:

| Action | Effect |
| --- | --- |
| `/extensions` enable or disable | Tool array changes |
| MCP `notifications/tools/list_changed` | Tool array changes |
| `subagent_spawn` with `tools: ["read"]` | Child gets a different array than parent |
| Server restart / refresh | Tool array changes |

octet has no mechanism that keeps the array stable or small, so each of these is
a full cold miss. Codex has two: deferred exposure, and catalog revision
freezing.

### What octet does today

- `DEFAULT_TOOL_SCHEMA_BUDGET_BYTES: Option<usize> = None`
  (`crates/octet-agent/src/agent.rs:502`) — no byte quota, and no deferral
  concept. Every enabled builtin, every `octet-mcp` server tool, and every
  `octet-computer-use` tool is advertised directly, every request.
- `octet-mcp` is a resident Python `BridgeManager` that republishes server tools
  as octet tools. It handles `notifications/tools/list_changed`
  (`extensions/octet-mcp/octet_mcp/protocol.py:807`) and per-server
  `refresh`/`restart`, but nothing pins a catalog revision to the model-visible
  list for cache purposes.
- `octet-computer-use` "republishes a reviewed subset" of Cua Driver tools as
  octet tools (`extensions/octet-computer-use/README.md`). Permanent schema
  cost for a capability most sessions never invoke.

### What Codex does

Deferred exposure, with a search surface:

```rust
// codex-rs/tools/src/tool_executor.rs:51
pub enum ToolExposure {
    Direct,            // in the initial model-visible list
    Deferred,          // registered, omitted from initial list, search-discoverable
    DeferredModelOnly, // search-discoverable, not callable from code mode
}
```

`ToolSearchInfo` (`codex-rs/tools/src/tool_search.rs:51`) holds an
`Arc<ToolSpec>` against an immutable catalog snapshot, so discovery does not
duplicate schemas.

Frozen catalog revisions on the MCP side. The module doc of
`codex-rs/codex-mcp/src/client_tool_catalog.rs` states the policy:

> Only active readers, calls, and frozen model bindings pin earlier snapshots.
> … Ordinary servers coalesce list-change notifications and publish only a
> stable listing. … Failed refreshes retain the last catalog and share a short
> retry cooldown.

with `NOTIFICATION_REFRESH_COOLDOWN = 5s`.

Net effect: Codex ships a small, stable tool prefix and lets the model search
when it needs something outside it. octet ships everything and keeps reshuffling
it.

## Second leak: compaction threshold scope

octet's budgeting itself is sound — whole-request estimate, 16K reserve, recent
tail retention:

- `DEFAULT_COMPACTION_RESERVE_TOKENS = 16 * 1024`
  (`crates/octet-agent/src/agent.rs:446`)
- `DEFAULT_KEEP_RECENT_TOKENS = 20_000`
  (`crates/octet-agent/src/compaction.rs:28`)

But the threshold counts the **whole** active context. A large system prompt
plus a large tool array counts fully toward triggering compaction, so the stable
cached prefix pushes a session toward compaction earlier than the growing part
justifies.

Codex splits the threshold by scope:

```rust
// codex-rs/core/src/session/context_window.rs:62
AutoCompactTokenLimitScope::Total => (active_context_tokens, ...),
AutoCompactTokenLimitScope::BodyAfterPrefix => {
    let baseline = window.prefill_input_tokens.unwrap_or(active_context_tokens);
    (active_context_tokens.saturating_sub(baseline), ...)  // growth only
}
```

`BodyAfterPrefix` measures growth *after* the stable initial prefix, so the
already-paid, already-cached prefix stops counting against the trigger. octet has
no equivalent.

Related Codex bounds worth comparing against octet's `2 MiB` handoff cap
(`MAX_COMPACTION_HANDOFF_BYTES`, `crates/octet-agent/src/compaction.rs:40`):

| Constant | Value | Location |
| --- | --- | --- |
| `RETAINED_MESSAGE_TOKEN_BUDGET` | 64_000 | `core/src/compact_remote_v2.rs:95` |
| `COMPACT_USER_MESSAGE_MAX_TOKENS` | 20_000 | `core/src/compact.rs:68` |
| `MAX_RETAINED_AGENT_MESSAGE_TOKENS` | 10_000 | `core/src/compact_pending_input.rs:10` |

A byte cap is not a token cap: 2 MiB of dense prose and 2 MiB of short lines land
far apart in tokens.

## Third leak: image token accounting (correctness, not just cost)

octet estimates media with flat constants
(`crates/octet-agent/src/agent/context_estimate.rs`):

- `ESTIMATED_IMAGE_TOKENS = 1_600`
- `ESTIMATED_AUDIO_TOKENS = 8_000`
- `ESTIMATED_DOCUMENT_PAGE_TOKENS = 3_000`

Codex estimates from patch geometry, with a cache
(`codex-rs/core/src/context_manager/history.rs:1331-1340`):

```rust
const RESIZED_IMAGE_BYTES_ESTIMATE: i64 = 7373;
const ORIGINAL_IMAGE_PATCH_SIZE: u32 = 32;
const ORIGINAL_IMAGE_MAX_PATCHES: usize = 10_000;
const ORIGINAL_IMAGE_ESTIMATE_CACHE_SIZE: usize = 32;
```

A 2560×1440 screenshot is nowhere near 1,600 tokens. This matters most in the
exact workflow computer use creates: an agent takes a dozen screenshots, octet
believes it spent ~19K tokens, the real cost is several times that, and the
request overflows the window with a misleading footer percentage. Codex also
gates encode on `context_manager/media_availability.rs` before spending the
bytes.

## Area-by-area comparison

### Subagents / delegation

octet is **ahead** on durability, and this should not be regressed:

- Depth-1, 16 active / 32 retained per owner.
- Host-owned child conversation; extension owns only decomposition and completion
  policy.
- Session-scoped, not run-scoped: the run detaches and the worker's record
  persists (`docs/subagents.md`, "Session-scoped delegation").
- Durable roster `fleet-<owner hash>.json` beside a root-scoped lease;
  `run_detached` boundary; ledger entries settle without reserving slots.
- Opaque `agent-session:<sha256>` resume handle resolved by
  `SessionStore::path_by_id` with typed refusals
  (`DelegatedHandleRefusal`).
- Per-worker `provider` / `model` / `reasoning`, resolved fail-closed with no
  parent fallback.
- `crates/octet-agent/src/delegation/` is ~16.3K lines including tests.

Codex is ahead on **declarative roles**: `codex-rs/agent-roles/src/` provides
`loader.rs`, `discovery.rs`, `agent_role_config.rs` — roles are config files, so
a user can define a new worker shape without a code change. octet hardcodes four
profiles (`explore`, `research`, `review`, `test-analysis`) in the tool schema.

Codex also has agent-to-agent messaging (`agent-message-board-host`,
`agent-message-board-client`) and a graph store (`agent-graph-store`). octet
deliberately excludes team chat and graph orchestration. **This is a design
choice worth keeping**; it is not a parity gap.

One real subagent cost problem: children inherit the parent's full standard tool
scope by default, so a mutating child re-sends the whole schema array on its first
request regardless of whether it will write anything.

### Computer use

octet **has the capability**; Codex does not, natively. octet bridges to the
MIT-licensed Cua Driver over local stdio MCP and republishes a reviewed tool
subset. Codex instead routes GUI work through a `node_repl` MCP connector and
gates it with a per-model flag read off model metadata — `computer_use_review_required`
in `codex-rs/core/src/turn_metadata.rs:76`, surfaced as
`node_repl_auto_review_required`.

So the gap is not the driver. It is:

1. Permanent schema cost for a rarely-used capability (fix: deferred exposure).
2. Underestimated screenshot tokens (fix: patch-based estimation).

`docs/design/browser-computer-use-redesign.md` covers the driver-side redesign;
this document covers only the context-accounting side.

### Code mode / codemode

Both have it, with different runtimes:

- octet: `extensions/octet-codemode`, vendoring Pi's MIT-licensed QuickJS/WASM
  guest. Host limits: 256 calls, 30s, at most 4 parallel observations, mutations
  exclusive (`docs/tools.md:69-91`). `on` keeps direct tools, `only` makes
  ordinary tools nested-only.
- Codex: `codex-rs/code-mode`, `code-mode-host`, `code-mode-protocol`,
  `code-mode-runtime`, as a gRPC host process with
  `CodeModeSessionCellExecutionLimits` and `CodeModeOutputTruncation`
  (`core/src/code_mode_host.rs`).

Feature parity is close. The structural difference is that Codex's code mode
consumes the same `ToolExposure` axis, so a `Deferred` tool is omitted from the
initial list *and* still reachable from inside the guest. octet has no exposure
axis to share.

Also worth noting: octet's nested `bash` already returns independently bounded
`stdout` / `stderr` with byte counts and completeness flags, which is better than
what the direct tool surface gives the model.

### MCP

Codex's MCP is a subsystem; octet's is one extension process. Not a fair
line-for-line comparison, but three properties are worth adopting at any
implementation size:

| Property | Codex | Why it matters |
| --- | --- | --- |
| Required-server semantics | `codex-mcp/src/connection_manager/required.rs` | A required server failing is a startup error, not a silently empty catalog |
| Bounded stdio transport | `rmcp-client/src/bounded_stdio_transport.rs` | One oversized frame from a hostile server cannot OOM the host |
| Elicitation | `codex-mcp/src/elicitation.rs` | Mid-call server→client prompts instead of fail-and-retry |

octet's MCP does have per-server lifecycle, a private `0600` config, schema
validation, and a trusted-project distinction — that part is sound. What it lacks
is the catalog-revision discipline that keeps the model-visible list cacheable.

## Where octet is ahead — do not regress

`crates/octet-agent/src/cache_warmer.rs` has no Codex analogue and is a better
cache story than relying on provider-side storage:

- Timer at 90% of the declared TTL, keeping at least 10s of margin.
- `streaming` profile: 1h horizon, 100% continuation probability. `idle`: 30m,
  15%.
- Economic gate: `MINIMUM_SAVINGS_MICRODOLLARS = 50_000` ($0.05).
- A refresh replays the **exact request snapshot**, changing only the output cap
  to one — no synthetic suffix, no breakpoint move, no conversation mutation.
- Delegated children share the live user-selected mode.

That mechanism composes with, and partially masks, every issue above. Fixing the
prefix problems makes it more effective, not less.

## Proposal

Ordered so that each step is independently shippable and step 4 is a
prerequisite for measuring step 1.

| # | Change | Cache impact | Effort |
| --- | --- | --- | --- |
| 1 | Deferred tool exposure + `tool_search` | Prefix drops from ~150 schemas to ~6 and stops churning | High — largest win |
| 2 | `body_after_prefix` compaction scope | Long sessions keep a warm prefix | Medium |
| 3 | Patch-based image token estimation | Correctness for computer use | Low |
| 4 | Freeze tool array per episode; coalesce `list_changed` | Removes mid-session cold misses | Low |
| 5 | Declarative subagent roles as config files | None | Medium |
| 6 | Token-oriented truncation policy | ~10–20% output token savings | Low |

### 1. Deferred tool exposure + tool search

Add an exposure axis to the tool registry mirroring
`ToolExposure::{Direct, Deferred}`. Keep `read`, `edit`, `write`, `bash` (and
`powershell` on Windows) `Direct`. Default `octet-mcp` server tools and
`octet-computer-use` tools to `Deferred`. Ship one `tool_search` tool that
returns matched specs on demand.

Acceptance criteria:

- A session with an MCP server configured sends no MCP tool schema in its first
  request.
- `tool_search` can surface a deferred tool, and the surfaced spec is
  byte-identical to what direct exposure would have sent.
- Enabling an extension mid-session does not change the advertised array for the
  current episode.
- A deferred tool remains reachable from inside `octet-codemode` when code mode
  is `on`.

Risk: a model that does not search will fail where it previously succeeded. Ship
behind a setting, and measure task completion before and after. Do not default
`Deferred` on for the core five tools.

### 2. `body_after_prefix` compaction scope

Record `prefill_input_tokens` at episode start and add a scope option that
measures `active_context_tokens - prefill_input_tokens` against the threshold.
Keep `total` as the default initially; flip after measuring.

Acceptance criteria:

- With `body_after_prefix`, a session whose prefix is 180K of a 200K window can
  still add meaningful turns before compacting.
- The footer percentage keeps using the full model window (existing contract,
  `docs/context.md:102`), and scope does not change it.

### 3. Patch-based image token estimation

Replace `ESTIMATED_IMAGE_TOKENS = 1_600` with dimension-derived estimation at the
model's patch size, cached by content hash, plus downscale-before-encode. Keep
`ESTIMATED_AUDIO_TOKENS` and `ESTIMATED_DOCUMENT_PAGE_TOKENS` as-is unless
measurement says otherwise.

Acceptance criteria:

- A 2560×1440 PNG estimates within a defined tolerance of the provider's reported
  input delta.
- Estimation is cached: repeated reads of the same image do not re-decode.
- An estimated-overflow request fails closed before dispatch, rather than being
  discovered as a provider rejection.

### 4. Freeze the advertised array per episode

Resolve the model-visible tool list once per episode; only re-resolve at an
explicit turn boundary or user action. Coalesce `notifications/tools/list_changed`
into a stable listing with a short cooldown, and retain the last good catalog on a
failed refresh.

Acceptance criteria:

- A `list_changed` burst produces at most one re-advertisement per cooldown
  window.
- A failed refresh leaves the previous catalog intact and does not empty the tool
  list.
- Prompt-cache hit rate is measurable per session, so step 1 can be evaluated.
  **This observability is the actual prerequisite** — add it first if the rest
  must be sequenced.

### 5. Declarative subagent roles

Move the four hardcoded profiles to config-file definitions loaded at startup,
carrying allowed tools, default model/reasoning, output ceiling, and a
description. Keep the existing profiles as built-in defaults so behavior is
unchanged when no config is present.

Also consider defaulting `profile: explore` to `tools: ["read"]`, which is both a
safety improvement and a schema-cost reduction for the common case.

### 6. Token-oriented truncation policy

Replace byte-only `max_output_bytes` with a policy enum that can express either
bytes or tokens, add a serialization allowance, and make truncation legible to the
model. Codex's shape
(`codex-rs/utils/output-truncation/src/lib.rs:23-45`):

- `with_serialization_allowance(policy) = policy * 1.2`
- Header: `Warning: truncated output (original token count: N)` plus total line
  count.

The model currently receives a byte-truncated blob with no statement of what it
lost, and octet's spill files (`docs/tools.md:174-214`) are invisible to it. A
token budget with an explicit loss header is both cheaper and more honest.

## Non-goals

- Team chat, mailboxes, graphs, or swarms for subagents. octet's deliberate
  exclusion stands.
- Replacing the Cua Driver integration. `docs/design/browser-computer-use-redesign.md`
  owns that.
- Porting Codex's OAuth, entitlement, or EMM machinery.
- Changing `cache_warmer.rs` behavior. It is ahead of Codex and orthogonal.

## Open questions for the code owner

1. Where does the advertised tool array get materialized, and is it already
   resolved once per turn or re-derived per request? Step 4's cost depends on
   this.
2. Does the provider request builder preserve tool order deterministically today?
   If not, that is a latent cache bug independent of everything above.
3. Is `prefill_input_tokens` already recorded anywhere in the session, or does
   step 2 need a new session record?
4. For step 1, is the blocker in the Rust tool registry or in the Python
   `octet-mcp` republishing path? The cleanest split is exposure decided in Rust,
   catalog supplied by the extension.
5. Can cache hit rate be derived from existing provider usage fields, or does it
   need a new telemetry dimension?
