# `octet-coding-agent` design

**Status:** Current implementation contract.

`octet-coding-agent` is the product layer over `octet-ai` and `octet-agent`. It owns
configuration, provider authentication, session discovery, compaction policy,
skill discovery, and the interactive/plain/print frontends. It does not
reimplement the model/tool loop.

The shared customization discovery, trust, limits, diagnostics, precedence,
and reload contract is documented in [`../resources.md`](../resources.md).

## Build and dependency boundary

The workspace MSRV is Rust 1.86. `sexy-tui-rs` is vendored as
`crates/sexy-tui-rs`; builds must not depend on a sibling checkout. Its import
provenance is recorded in `crates/sexy-tui-rs/VENDORED.md`.

## Offline migration inventory

The `migrate pi` top-level command is dispatched before normal product
configuration and bootstrap. It reads bounded Pi settings/package resources,
parses extension source with a real TypeScript syntax tree, and emits a
versioned dry-run report. It never constructs an `Agent`, discovers a provider,
executes package code, writes either setup, or invokes a model. A malformed
package becomes a local diagnostic rather than preventing independent packages
from being inventoried. The user contract and future compatibility boundary are
documented in [`../pi-migration.md`](../pi-migration.md).

## Startup and resume

Interactive terminal ownership precedes provider-catalog bootstrap. Cold or
expired model discovery runs through the same blocking-lifecycle worker boundary
as session/extension startup, so typing, paste, resize, and coordinated Ctrl-C
shutdown remain live while discovery waits. The pending surface shows the draft
without a provisional model footer; branded readiness and prompt submission
still wait for launch resolution, extension startup, and history hydration.
Plain, print, and RPC retain their non-TUI bootstrap path. This is a responsiveness
boundary, not a reduction in provider latency or native-history replay work.

Startup resolves the persistent session before final model selection:

1. Select a new, latest, named, or interactively picked session, or fork a
   selected source into a new session before replay.
2. For an existing session, walk its active parent chain and recover the newest
   model and reasoning values from `EntryValue::Config` records.
3. Explicit `--model` and `--reasoning` flags override recovered values.
   Project/global defaults apply only when the session has no corresponding
   value.
4. Resolve the model before filling an unset reasoning preference from its
   advertised default (or first supported enabled choice). Explicit Off remains
   distinct from unset; absent usable reasoning metadata stays Off. Normalize
   the selection against the model's capabilities. A persisted legacy Pro bit
   migrates to Ultra only when the
   route advertises Ultra effort and V2 collaboration and the host has an
   executable V2 runtime; otherwise it is cleared with a warning while the
   independently selected effort is retained.
5. Append the effective configuration as provenance before constructing
   `Agent`, except when an existing session already ends in the same marker.

Runtime `/resume` and branch checkout use the same restoration behavior.
Interactive resume follows renderer ownership. Default terminal-owned mode
hydrates the complete active branch so Pi's complete logical frame can populate
native scrollback without an impossible later prepend. Explicit application-owned
mode hydrates only a bounded active-branch tail for first paint; the complete
branch is materialized when semantic navigation or selection reaches beyond that
tail.

Session discovery uses a workspace-local disposable SQLite projection of
bounded active-branch titles and message counts keyed by transcript size and
modification time. The resume picker can lazily enumerate all workspace
subdirectories under the shared session root; `.workspace` markers provide
canonical display paths while JSONL and `.metadata/` remain authoritative.
JSONL remains authoritative: cache misses and stale fingerprints are streamed
under the same byte/record bounds, cache failures fall back to JSONL, and
normally dropped `App` instances refresh their already-replayed active session
without reopening it. The catalog is never a prerequisite for resume.

## Provider setup and readiness

The custom credential registry and the canonical `ModelCatalog` are the sole
provider state. `ProviderSetupService` captures a registry snapshot, validates
one explicitly selected OpenAI-compatible endpoint, optionally performs one
bounded `/models` probe, presents a secret-free receipt, and persists through a
private compare-and-swap only after final confirmation. It then rebuilds the
same canonical catalog and verifies the selected `ModelId`; it never constructs
a partial `Agent`, scans localhost, follows discovery redirects, writes setup
telemetry, or creates a parallel catalog/store.

Interactive startup opens the ordinary guided setup surfaces only when bootstrap
has no runnable model inventory and there is no explicit `--model`; cancellation
continues in existing read-only model-less mode. A successful setup installs the
rebuilt catalog and selected default in memory, while keeping resumed-session
model provenance eligible to win under normal startup precedence. Print and RPC
never open a picker: unresolved and unavailable models return a deterministic
secret-safe diagnostic that names `octet setup --yes` and available model IDs.

The non-interactive `octet setup` adapter shares the transaction. It requires an
explicit `--endpoint` or explicit `--preset lm-studio`, reviews by default, and
uses `--yes` to commit. `--offline --manual-model` is the no-probe recovery
path. Cancellation, review-only operation, offline discovery rejection, and a
stale registry snapshot do not write the registry.

## Custom endpoint lifecycle feedback

A custom registry provider may set `lifecycle_feedback: true`. Bootstrap copies
that explicit opt-in into the selected route's `RequestRuntime`; legacy and
built-in declarations remain false. The HTTP client then owns negotiation and
parsing, so product configuration never changes an ordinary endpoint's wire
behavior.

The product treats accepted readiness updates as nonpersistent activity. The TUI
replaces its mutable `Working` row with a bounded provider-status label until
model output or settlement; plain and print modes write the diagnostic to stderr
so print stdout stays response-only. RPC and the native host expose a structured
`provider_lifecycle` event for clients that choose to render it. Session records,
serve item projections, and durable telemetry deliberately omit this endpoint
telemetry. It is observational only and does not change retry or timeout
semantics.

## System prompt

The stable, model-agnostic base contract gives both local and cloud models an
explicit completion trajectory: honor answer/investigate/review/plan/implement
mode and treat the latest explicit process constraint as authoritative—an instruction
to answer now or stop using tools ends further investigation; use tools rather than
guess when tools remain permitted; inspect before editing; continue while additional
work can materially improve the requested result; proceed autonomously with local,
reversible work while
confirming destructive, hard-to-reverse, outward-facing, or remote/shared-state
actions that were not explicitly authorized; preserve unrelated work; deliver
the requested scope without silently narrowing or widening it; make the
smallest complete change; verify the diff and relevant checks; and lead with
concise observed results and `path:line` references. It forbids commits unless
requested and makes clear that supplied tool schemas are authoritative.
Repository content, tool output, and external content remain data rather than
instructions; project or skill guidance is authoritative only when the host
labels it as such.

System instructions are composed through `compose_instructions(&Config)`.

- If `Config::system_prompt` is `Some(value)`, composition returns that exact
  value (including `""`) and bypasses AGENTS/context composition. Bootstrap may
  still append the discovered skill metadata and file-location catalog.
- If `system_prompt` is `None`, the default flow composes the base prompt and
  trusted workspace/global `AGENTS.md` context; bootstrap then appends the
  discovered skill catalog. It does not inject full skill bodies.
- Layer precedence for `system_prompt`, lowest to highest, is global config,
  trusted project config, `OCTET_SYSTEM_PROMPT`, then CLI `--system-prompt`.
  An explicit empty CLI value overrides every lower layer.
- The override does not persist through session metadata; startup and rebuild
  compose instructions from the current live config across `interactive`,
  `plain`, `print`, and `rpc`.

The environment block truthfully distinguishes the workspace root from the
invocation directory. Relative tool paths and the default `bash` working
directory resolve from the workspace root. Enabled core-tool names are listed;
extension tools may be supplied alongside them. Skill discovery is represented by
file locations in the composed prompt, not a built-in model-facing tool surface.
When the workspace has the octet source-checkout markers, the base prompt
also includes absolute paths to the README, `docs/`, `examples/`, `crates/`, and
the coding-agent crate and tells the model to consult them for octet questions or
changes. Packaged installs resolve the matching README, docs, examples, and
SDK assets beside the binary or under `share/octet`; Cargo installs materialize the
text assets from the binary into that same data layout. Behavioral changes
require regression tests rather than model-specific prompt tuning.

Global and trusted workspace `AGENTS.md` files retain root-to-leaf precedence
and are wrapped in path-labelled `<project_instructions>` blocks. Skill discovery
adds only its bounded metadata and file-location catalog to the composed prompt;
full skill bodies are not injected by this composition.

## Compaction and handoff summaries

Before installing the agent policy, bootstrap combines the generic fractional
threshold with an optional absolute active-context ceiling. Codex request
budgeting caps most models, including Astra, at 272,000 tokens; GPT-5.6 Luna may
use up to 372,000 tokens. Smaller provider windows remain authoritative, while
larger advertised maxima remain discovery metadata. An explicit
`compaction.max_active_tokens` further constrains the working set; zero or unset
removes only that additional absolute ceiling, not the model-specific Codex
route cap. `threshold_fraction` remains authoritative. The lower effective
threshold is applied on initial construction, rebuild, interactive
reconfiguration, and RPC toggles, and `/context` reports that same effective
capacity.

The product pre-request gate and `octet-agent` overflow recovery share one
Pi-compatible summarization implementation. Conversation messages are first
serialized inside `<conversation>` tags so the model cannot mistake them for a
live turn. Initial and iterative summaries use Pi's exact structured Markdown
contracts; iterative calls provide the prior checkpoint in
`<previous-summary>` tags. Branch-handoff helpers use Pi's corresponding branch
prompt and preamble.

File tracking is deterministic host behavior, not model output. Successful or
failed assistant calls to `read`, `write`, and `edit` contribute paths;
modified paths supersede read-only paths; and deduplicated sorted lists are
appended as `<read-files>` and `<modified-files>` blocks. The cumulative
`readFiles`/`modifiedFiles` details are persisted on compaction entries so later
summaries retain them. Legacy entries deserialize with empty details.

## Agent construction and tools

Every build or idle-boundary rebuild creates one `ExtensionHost`. `CoreTools`
registers the core tools in order: `read`, `edit`, `write`, `bash`, then opt-in
`search`; the default surface omits `search` because `bash` already provides
`rg`/`find`/`ls`. Enabled extensions may add their own tools after discovery and
policy admission. Skill discovery does not add a model-facing tool.

Context budgeting reserves the serialized schemas from that exact host rather
than reproducing a hard-coded subset. When delegation is installed, bootstrap
recomputes the reserve from `agent.system_prompt()` and
`agent.registered_tool_definitions()` so its instructions and schemas count.
A consuming rebuild drops the old Agent before reopening its session, so only
one append handle owns a session file. Every Agent also receives a product-owned
`EffectBroker`. Its default uses `UnsafeHost`, where authoritatively classified
effects use the current user's host authority subject to the remaining tool and
sandbox gates. `effect_policy` / `OCTET_EFFECT_POLICY` / `--effect-policy` select
`controlled_bash_approval`, `controlled`, or `unsafe_host`; a trusted project may
tighten, but not relax, the global profile, while environment and CLI use normal
precedence. `--safe-mode` conflicts with `--effect-policy` and selects
`ControlledBashApproval`, where workspace mutation and every `bash` process call
are approved interactively while other ambient host/process, network, delegation,
and extension effects remain denied. Invalid profile errors are generic and never
echo the supplied value. Executable-extension startup is gated separately at the
product boundary: even enabled, trusted extensions are discovered but never
launched under `--safe-mode`. Delegated children inherit the same broker policy
through the root's delegation template.

For explicit capability/orchestration boundaries (search vs browser vs computer use, hosted vs in-harness delegation, trust/cwd/approval/sandbox inheritance, and scope non-goals), see [`docs/design/extension-capability-and-orchestration-boundaries.md`](extension-capability-and-orchestration-boundaries.md).

Serve currently has no policy-decision item in its graphical protocol, so its
projection intentionally ignores `ToolPolicyDecision`; the matching
`ToolFinished` remains visible. Exposing policy evidence there requires an
explicit Serve protocol addition rather than silently changing the projection.

The coding host creates an extension-only V2 delegation manager whenever the
trusted, enabled `octet-subagents` extension successfully negotiates its
`agent_sessions` service. The manager is available at every reasoning effort so
client-level child work uses the same observed tree; it never installs the
native root collaboration tools or their hidden prompt instructions. Ultra is
selected only when that service is live and the model also advertises Ultra plus
V2 delegation. Without the service, Ultra is clamped to the highest ordinary
safe effort. The coding host chooses this activation policy, while `octet-agent`
enforces execution, isolation, provenance, limits, and cancellation;
`octet-ai` only reports the provider capability. Delegated children inherit the
root's approved extensions, sandbox, model/reasoning and cache settings,
compaction/completion/output policy, retry and turn bounds, and cost ceiling.
Their bounded status, spawn/list result, and durable spawn record include the
same effective tool-policy snapshot plus source-only parent-inherited versus
child-override orchestration provenance; they never expose paths, environment
values, approval material, extension identities, or model arguments.
During an active interactive run, the product schedules one nonblocking
owner-scoped subagent status refresh every 250 ms, reduces the resulting fenced
semantic snapshot, and renders the complete bounded worker roster as one
persistent transcript event above the composer. Ordinary tool disclosure never
truncates that event. It temporarily adds structured priced child cost to the
host-owned footer; after `octet-agent` mirrors the settled child usage into root
`delegated_agent` records, the idle footer reads only the durable session total.

## Skills

Skills are discovered from user, workspace, and explicit CLI directories with
explicit paths taking highest precedence. Workspace skills require workspace
trust. Discovery supplies immutable descriptors: the composed prompt lists each
visible filesystem skill location and directs the model to use the ordinary
`read` tool. No model-facing skill-specific tools are registered, and discovery
does not inject full skill bodies into the system prompt.

The frontends have different command and persistence paths:

- TUI `/skills load NAME` resolves the skill and prefills `/skill:NAME`. On
  submission, `expand_skill_command` checks declared required tools and inlines
  the `SKILL.md` body plus arguments into an ordinary user message; it does not
  append `SkillActivated`. TUI `/skills off` can append `SkillDeactivated` only
  for an activation already present on the branch.
- Serve's slash-command worker appends `SkillActivated` (descriptor, content
  hash, and instructions) on load and `SkillDeactivated` on off. The activation
  event is Serve-only; TUI `off` can only deactivate pre-existing state. Plain,
  print, and RPC do not gain this activation path.
- Interactive submission and the plain, print, and RPC prompt paths expand
  explicit `/skill:NAME` text as ordinary prompt content. They do not provide a
  second activation API. Prompt-template `{{skill:name}}` expansion likewise
  requires an already active session skill.

The model uses ordinary `read` for `SKILL.md` and referenced files, subject to the
normal allowlist and sandbox. For package resource semantics, supporting text is
limited to `references/` or `templates/`. Discovery caps YAML frontmatter at 32
KiB and `SKILL.md` at 256 KiB; a supporting text read is capped at 512 KiB. The
session schema contains `SkillResourceRead`/resource snapshots, but no current
registered model tool writes that event; it is not a model-facing resource API.

Compaction keeps the configured recent-message token window (default 20,000) and
summarizes older ordinary messages. A TUI inline body is therefore ordinary
history and may be summarized away; it does not become durable active state.
Serve activation events are separate session state and their active snapshots
are carried by compaction, so that Serve-only persistence must not be promised
for TUI activation.

## Prompt templates

Prompt templates are discovered from `~/.octet/prompts/`, trusted
`.octet/prompts/`, and repeatable `--prompt-template <path>` sources. Explicit
paths have highest precedence. Markdown files use Pi-compatible YAML
frontmatter and `$1`/`$@`/default/slice arguments; octet also accepts a small
TOML form and deterministic `{{prompt}}`, `{{workspace}}`, `{{selection}}`,
`{{file:path}}`, and `{{skill:name}}` variables.
Interactive invocations resolve `{{selection}}` from the current semantic
transcript selection without copying to or reading from the system clipboard;
startup and print-mode invocations expand it to an empty string.

Template and included-file reads are bounded, traversal is rejected, and final
expansion is capped before provider submission. Each selection persists its
name and SHA-256 in a non-model-visible session entry. Use `/prompt` to inspect
templates, `/prompt <name> ...` or Pi-compatible `/<name> ...` to invoke one,
and `--prompt <name>` for a startup/print prompt. `--debug-prompt` exposes the
exact deterministic expansion and template hash before provider submission.
Pi `argument-hint` metadata appears in slash autocomplete but is never inserted
into the composed prompt.

## Interactive commands

Commands run immediately when safe or queue to the next idle boundary when they
need Agent/session ownership.

octet uses one compiled default theme with built-in `auto`, `light`, and `dark`
terminal appearances. Theme-file customization and filesystem theme discovery
remain disabled; the appearance selector is not a theme loader. See
[Theme status](../themes.md).

- `/theme [auto|light|dark]` — inspect or select a built-in terminal appearance.
- `/model [id]` — pick or select a model.
- `/thinking [level]` — select a capability-gated reasoning level.
- `/answer [instruction]` — persist an answer-now steering message and switch the
  active run to tool-free requests at its next safe boundary; when idle, begin a
  tool-free run directly. Both paths set `ToolChoice::None` and expose no tool
  schemas while preserving ordinary session and cancellation semantics.
- `/verbose [on|off]` — expand/collapse every tool panel.
- `/compact` — force a compaction attempt.
- `/reload` — reload AGENTS instructions, prompts, skills, and extensions.
- `/new`, `/resume [id]` — switch persistent sessions; the resume picker supports
  fuzzy/phrase/regex filtering, named-only filtering, recent/name/message-count
  sorting, current/all-workspace scopes, rename, and recoverable trash.
- `/fork` — fork from a selected user message (or the whole current conversation)
  and prefill that message in the new session; `/clone` forks at the current head.
- `/tree`, `/checkout <entry-id>` — inspect durable entries and switch branches.
- `/name [name]`, `/export [path]` — name and safely export the current session.
- `/prompt [name] [arguments]` — inspect or expand prompt templates.
- `/skills search|load|reload|off ...` — inspect, invoke, reload, or deactivate skills; TUI load-prefill does not establish durable activation.
- `/extensions [status|reload]` — interactively enable/disable managed executable bundles, inspect diagnostics, or reload running full-access extensions; enablement never grants trust and safe mode keeps processes stopped.
- `/subagents` — when supplied by the enabled `octet-subagents` package, navigate workers with arrow keys and open owner-authorized read-only transcripts with Enter.
- `/help [command]` — show local command help and octet self-documentation.
- `/status`, `/quit` — product status and lifecycle controls.

The top-level `octet doctor` command performs read-mostly prerequisite, provider,
and model-visibility checks without constructing an Agent or starting executable
extensions. `--telemetry PATH` is an explicit opt-in for machine-readable
request/tool/compaction measurements; it is not part of the durable session
schema and is not rendered as one transcript row per event.

Checkout appends a durable head record. The subsequent consuming rebuild
restores configuration on the selected branch and appends current provenance;
future messages therefore fork without deleting the abandoned branch.

`/skills` also reports bounded discovery and validation diagnostics from the
current reload generation. A malformed manifest, rejected link, or ID mismatch
does not prevent healthy skills from loading and no longer disappears into
startup-only stderr.

The extension menu enumerates managed executable bundles rather than the
separate `octet-serve` application. It edits only the selected name in the user
config's `enabled_extensions`, preserves independent trust grants and unrelated
activation, refuses to redirect a shadowed global bundle to project/explicit
code, and performs a full idle-boundary rebuild so the new process set is live.
If project, environment, or command-line activation contributes to the effective
list, the menu remains inspectable but read-only because a global edit would not
be authoritative for the next launch, and trusted-project precedence is
revalidated at action time. Enabled unavailable bundles are disable-only;
source-changing trust, tool collisions, and explicit required-tool removal fail
closed.

## OpenAI Codex discovery and Ultra

Authenticated Codex discovery sends compatibility client version `0.153.2` and
parses the provider's string/object reasoning levels, `use_responses_lite`, and
`multi_agent_version: "v2"`. Cache schema version 6 invalidates inventories
queried with older compatibility or context-window policies. It preserves those
fields, uses a 372K working window for GPT-5.6 Luna and 272K for other Codex
models, and remains scoped to the authenticated account context. Only fresh,
complete, account-matched metadata is registered. Stale or future-dated cache
entries are refreshed synchronously before online catalog construction;
malformed, incomplete, duplicate, or inconsistent entries fail closed. Offline
launches may retain fresh cached model identities and limits but strip Ultra,
Responses Lite, and delegation. octet never infers those dynamic capabilities
from model names or OAuth plans. Astra additionally retains its 872K advertised
input maximum as discovery metadata while ordinary request budgeting stays at
272K.

Ultra is selectable only when a complete live-derived reasoning range
explicitly includes `ultra`, V2 delegation is present, and the linked
`octet-agent` host reports an executable V2 runtime. An advertised `max` level is
never promoted to Ultra. This avoids advertising “maximum reasoning with
automatic task delegation” when only the model-side maximum is present. The
legacy persisted `ReasoningMode::Pro` value remains readable for durable
sessions, but no picker or new configuration writes it and no codec serializes
`reasoning.mode`.
Eligible legacy selections migrate at startup to Ultra; ineligible ones retain
their effort with Pro cleared and a warning. At every idle rebuild boundary, an
explicit effort selection likewise supersedes and clears any restored legacy
Pro bit unless the caller explicitly selected a mode.

Routes advertising Responses Lite use the transport contract implemented by
`octet-ai`, including its ordinary and compact request shapes and advertised
parallel-tool-call bit. This product layer only discovers and propagates the
capability; it does not reconstruct the wire format or infer support from the
endpoint identity.

## OpenRouter Batch API

The top-level `octet batch` command is a non-interactive product boundary for
OpenRouter's asynchronous Batch API. It builds ordinary layered configuration
only to resolve invocation, provider, and offline policy, then selects an
OpenRouter model, reads bounded JSON from a file or stdin, submits native request
bodies, and emits sanitized JSON metadata or results. `batch get --wait` polls
until a terminal provider status; `batch list` exposes cursor, status, and
creation-time filters.

Online startup uses the provider inventory. Offline startup may reuse only a
fresh cache bound to the OpenRouter inventory URL and credential fingerprint;
an explicitly named OpenRouter slug can otherwise receive conservative
batch-only metadata. Batch requests never enter session persistence, the
interactive tool loop, or automatic submission retries. See
[`../openrouter-batches.md`](../openrouter-batches.md).

## Host-owned GitHub Copilot

GitHub Copilot is deliberately a host-owned provider rather than a generated
CLI preset. `CopilotHost` retains GitHub device-flow/OAuth credentials, token
exchange/refresh state, discovery transport, and any durable storage; octet
receives only a bounded device-display payload, safe availability result,
credential-free model metadata, and a short-lived in-memory inference session.
The public provider definition contains only route and setup metadata, and the
normal environment-backed bootstrap skips host-owned discovery entirely.

An embedding host explicitly binds a vetted origin root and calls
`CopilotProvider::register_models`. Registration checks availability, exchanges
a session, bounds and validates the authenticated inventory, selects Chat or
Responses routes from each model's declared protocol, rejects other protocols,
stages it, and fails closed on an existing route/model identity. Dynamic
credentials and headers remain behind
`octet_ai::Auth::Dynamic`; they are sensitive request headers and never enter
provider definitions, catalog model metadata, logs, or persistence. The resolver
serializes exchange/refresh and refreshes before its short-lived session expires.

The standalone CLI and NDJSON host intentionally provide no Copilot login or
configuration path. GitHub OAuth endpoint behavior, Enterprise policy, and live
integration testing remain embedding-host responsibilities; the product covers
only deterministic fake-host/transport behavior.

## Authentication

Codex OAuth credentials live in an owner-only directory and file. Writes use a
same-directory temporary file, flush and sync it, atomically replace the target,
and sync the directory on Unix. Token-bearing types redact both tokens from
`Debug`; token endpoint failures expose only status and a constrained OAuth
error code, never the raw response body.

Gemini Developer API presets are registered only while bounded `GEMINI_API_KEY`
is nonempty and use `x-goog-api-key` at request time. Vertex presets are
registered only after an owner-private, bounded Application Default Credentials
file and validated `GOOGLE_CLOUD_PROJECT` plus `GOOGLE_CLOUD_LOCATION` (or
`GOOGLE_CLOUD_REGION`) are present. The product accepts `authorized_user` and
PKCS#8 service-account ADC files, builds the regional Google authority from
single safe path segments, ignores credential-controlled token URLs, refreshes
into memory under a mutex, and sends refreshes only to the fixed Google OAuth
token endpoint with redirects disabled. It neither runs `gcloud` nor persists
ADC credentials or access tokens.

## Frontends

Interactive, plain, and print modes share `App`, `Agent`, session persistence,
reasoning normalization, and finish classification. TUI-specific rendering and
terminal ownership are specified in `docs/design/octet-tui.md`.
