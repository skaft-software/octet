# Pi extension compatibility: remaining work and Lean readiness

## Baseline and meaning of completion

Implementation checkpoint: `5165e670d6e415039a5a85180562e9cdec478d97` (local takeover commit).
This inventory does not claim full compatibility, release readiness or any Lean
proof. No Lean sources, Lake project, pinned Lean toolchain or Lean CI gate were
found in the candidate; neither Lean nor Lake was available on the inspected PATH.

The target inventory pins Pi 1.0.0 at
`581e7ba78141a4d8b61cc9d11b8b22ae7e59195e`. The adapter's selected Pi TUI
modules are 0.85.0: reconcile that profile rather than silently treating the two
versions as equivalent. The exact per-member/path backlog remains
[the machine inventory](pi-api-coverage.json), with its
[generated view](pi-api-coverage.md). This document inventories the remaining
workstreams and closure gates; it is not a claim that every dynamic behavior has
already been discovered.

“All Pi extension properties” needs an agreed observable contract: supported
versions, packages, frontends, platforms, success semantics, errors, ordering,
resource limits and security policy. Do not silently remove unsupported members
from that target. Any exclusion or deliberate incompatibility needs explicit
approval and must remain visible. Safe refusal proves safety, not Pi compatibility.
Arbitrary third-party JavaScript correctness, provider behavior and direct OS
side effects are not established by proving host-mediated extension contracts.

Keep these axes independent: implementation, public reachability, native testing,
unchanged-original acceptance, formal specification, proof, and implementation
refinement. A passing test is not a theorem; a theorem about a handwritten model
is not a proof of the deployed Rust/JavaScript executable.

## Current coverage accounting

Counts below apply audited row annotations over generated defaults. They are
structural findings, not distinct APIs, completed requirements or percentages.

| Structural category | Rows |
|---|---:|
| Members / nested fields / declarations | 4,913 / 667 / 1,035 |
| Events / context-registry members | 41 / 30 |
| Root exports / subpath exports / import paths | 1,770 / 1,606 / 342 |
| Observed upstream imports / original-corpus imports | 300 / 291 |
| Original-corpus call candidates / private assignments | 2,197 / 21 |
| Corpus profiles / explicit inventory gaps | 10 / 10 |
| Child exports / child paths / CLI paths | 40 / 4 / 2 |
| **Total** | **13,279** |

The inventory hashes 420 reference files and 67 candidate files. Its ten-package
original corpus hashes 722 files. Implementation classifications:

| Classification | Rows |
|---|---:|
| Unverified | 11,071 |
| Type-only reference | 1,148 |
| Missing shim export | 506 |
| Shim export present, unverified | 110 |
| Missing facade member | 97 |
| Source present, partial / mapped event, partial | 95 / 31 |
| Unsupported event / explicit refusal | 10 / 13 |
| Registration field rejected / accepted but semantics unverified | 10 / 9 |
| Constructor/static refusal, exact member parity unverified | 178 |
| Incompatible version profile | 1 |

The 506 missing-export rows represent 253 names across two package aliases
(65 AI, 135 coding-agent, 53 TUI). The 97 missing-facade rows include inherited
repetition. These classifications are not exhaustive body-level analysis:
exported functions can themselves refuse. Type erasure is not authoring-type
parity. Review each finding against actual source before assigning implementation
work; preserve unresolved dynamic imports, aliases and private patches as gaps.

Native evidence remains unverified for 13,275 rows; four have limited annotations
(three partial native passes and one mirror-transport pass). Original acceptance
also remains unverified for 13,275 rows; the other four contain partial evidence,
including a synthetic-host scenario. Synthetic evidence is unverified for 9,813
rows; the other 3,466 are lexical source candidates, not qualified test results.
The four annotated subjects are `waitForIdle`, `ui.custom`, `getBranch` and
`getEntries`. No row establishes full semantic qualification.

## Compatibility implementation and reachability backlog

Every item below remains open at some level. “Qualify” does not imply absent
implementation; explicit missing/refused behavior is distinguished from tests
still needed. Full signatures/options/overloads remain in the machine inventory.

| ID | Remaining work | Closure gate |
|---|---|---|
| C01 | Resolve all package roots, aliases, conditional/wildcard/subpath exports, type declarations and the 253 absent exported names; evaluate exported-but-refusing AI/TUI/SDK constructors and functions. | Actual unchanged imports and authoring typechecks under each pinned profile; no inference from a root alias or erased type import. |
| C02 | Resolve computed/dynamic imports, aliased call roots, transitive dependencies, private assignments and monkey patches. AST candidates are not exhaustive execution discovery. | Reviewed source graph plus scenario traces for every original; record residual uncertainty, exact dependencies and private-path obligations. |
| C03 | Expose Pi lifecycle signatures: `newSession`, `fork`, `switchSession`, `navigateTree`, `reload`, `shutdown`, `getSystemPromptOptions`; replace explicit `abort` refusal. | Bind actual owner-fenced native operations with setup/veto/replacement semantics, idle progress and cancellation. Native create/fork/switch/reload tests do **not** implement these absent Pi signatures. |
| C04 | Implement truthful context facts: `mode`, `thinkingLevel`, `scopedModels`, `isProjectTrusted`; tool-context `tools`/`executeTool`; `pi.exec` and `getSettings`. | Correct initialized snapshots and policy-admitted effects, including retained/replaced contexts. Direct OS execution is not a substitute for host approval. |
| C05 | Implement model/thinking setters, provider/virtual-model registration/removal and MCP enumeration/registration/removal. | Real catalog mutation, activation, stream/tool round trips, hook routing, cancellation and teardown under host authority. |
| C06 | Complete context ModelRegistry: typed lookups, provider/config/auth/status access, refresh/registration, `stream`, `streamSimple`, `complete`, `classify`, `generateImages`. | Specify credential access and inference policy; never fabricate secrets or silently start Pi's inference runtime. Existing snapshot getters are not full registry parity. |
| C07 | Implement readonly session tree/header/label/path/leaf and context/session projection members. | Faithful identity, parents, branches, namespace isolation and promised file semantics; oversize/unavailable mirrors must not reuse stale data. |
| C08 | Complete events, callback signatures, payloads, result transforms, ordering and failure policy. Ten events have no mapping (listed below). | Exercise actual host boundaries, not synthetic notifications; apply transformations at the correct pre-admission point. Qualify the 31 existing mappings too. |
| C09 | Complete tool registration fields, runtime registration, collision/override behavior, argument/loadout preparation and render invocation. | Pinned schema/option semantics, approved argument identity, active/default tools, native prompt/transcript behavior and output validation. |
| C10 | Implement `sendMessage` display/delivery and `sendUserMessage` options/media. Current `triggerTurn` uses a separate user-message injection, not proven Pi custom-message delivery. | Correct steer/follow-up/next-turn scheduling, template expansion, durable entries, visibility and no duplicate or wrong-session effects. |
| C11 | Complete provider/context/model-turn projection: audio, non-inline images/detail hints, opaque reasoning/continuations, tool scheduling/argument metadata, mixed entries and result-detail attribution. | Lossless representable round trips and explicit treatment of every remaining variant; native durable reload/checkpoint/provider tests, not only in-memory provenance. |
| C12 | Complete CLM native annotation, recall, context edits, checkpoints, next-context/provider flow, mixed/media/opaque continuity and preparation bounds. | Unchanged pinned CLM through actual App/Agent, durable reopen/branch isolation and real provider boundaries. Branch-settings restoration already passes; synthetic annotate/recall is not this gate. |
| C13 | Complete compaction/tree preparation fields and replacements, including `willRetry`, `tokensBefore`, settings/file operations, full tree summaries and metrics. | Real revision-bound anchors, veto/replacement/no-fallback behavior, durable commit before callbacks, explicit post-commit failure and checkpoint-size handling. |
| C14 | Supply compaction interaction without an active UI owner; qualify Headless/Serve and Native Responses paths. | Correct dialog availability or explicitly approved profile limitation; no fake confirmation/input or success on unavailable consumers. |
| C15 | Implement entry/message renderers, Markdown transformers, missing theme/chrome/working-indicator APIs and editor-component access. | Actual renderer invocation/replay and native presentation; OKLCH/OKHSL vector tests do not prove complete theme API parity. |
| C16 | Complete terminal-input observation/consume/rewrite, autocomplete-provider chaining, edit application and filesystem completion. | UTF-8 byte/cursor/revision fences, quote edits after the cursor, cursor placement, empty-result fallback and forced/automatic completion in the native frontend. |
| C17 | Complete custom-UI/dialog options, including `onHandle`, selection/confirmation options and input placeholder/options; qualify focus, mouse and retained UI races. | Actual frontend success/refusal, close acknowledgement before continuation, no unfocused composer ACK, terminal restoration and owner retirement. termDRAW acceptance covers only its observed scenario. |
| C18 | Qualify immediate custom-editor rescue and checkpoint/input/frame ordering on the current binary. | Original paced **and burst** PTY probes, crash/reload/switch, stale echoes/writes, mount replacement and last-committed-draft recovery. Historical paced success and synthetic ACK tests do not close this. |
| C19 | Complete resource discovery, precedence, theme loading, startup/reload/retirement and bus semantics. | Resolve Pi versus native precedence deliberately; qualify filesystem/trust admission and removal of contributions. Distinguish local synchronous object bus from host bounded session bus. |
| C20 | Define and implement legitimate general Pi child admission without weakening the first-party product gate. | Policy-approved product route; current `octet-subagents` admission is not authorization for arbitrary Pi factories. |
| C21 | Complete child SDK/CLI/session-file semantics: SessionManager, SettingsManager, DefaultResourceLoader, custom tools/model runtime, callbacks, overrides, mutations, argv/events and `@file` input. | Actual package/CLI consumers, bounded nesting, owner/budget/tool inheritance, cancellation, persistence, restart and exactly-once accounting. The bounded JSON-print CLI is not full Pi CLI parity. |
| C22 | Finish multilingual public SDK surfaces and their native transport/race coverage. | The language-specific matrix below; Rust internals or raw JSONL access do not establish an author API. |
| C23 | Close transport, cancellation, retirement, restart, quota and platform race matrices across all existing paths. | Deterministic barriers at both race linearizations, actual production ExtensionProcess, real executable SDKs and explicit missing-prerequisite failures. |

Unmapped events: `agent_before_settle`, `agent_settled`,
`cache_warming_decision`, `context_with_system`, `mcp_servers_change`,
`project_trust`, `provider_stream_event`, `session_before_fork`,
`session_before_switch`, `tool_execution_update`.

Rejected ToolDefinition fields: `annotations`, `constrainedSampling`,
`defaultActive`, `executionMode`, `exposure`, `namespace`, `outputSchema`,
`prepareArguments`, `prepareLoadout`, `renderShell`. The adapter accepts
`output_schema`, not pinned Pi's `outputSchema`. Accepted `renderCall` and
`renderResult` callbacks still need actual invocation/transcript qualification.

### Multilingual SDK closure

Use [sdk-parity.md](../sdk/conformance/sdk-parity.md) for the current author-facing
matrix and production-host evidence. Required work is not identical syntax across
languages; it is an explicit, tested supported-contract mapping.

| Surface | Remaining work |
|---|---|
| Python | Complete semantic/race/restart/platform qualification for existing author APIs; do not infer all hook/UI/provider/bulk paths from 43 native cases. |
| Rust | Commands, hooks, image/audio artifacts, general reverse services, dynamic catalogs, UI and lifecycle subscriptions are absent authoring surfaces. |
| TypeScript/JavaScript | Secure bulk file-I/O helpers; retained-owner/session-control/agent services; dynamic catalogs/UI/lifecycle subscriptions; full schema/union profile decisions. Native bulk/private-session hooks, retained services and full lifecycle races remain unqualified. |
| C ABI / C++ | Rich/nested input schemas, structured output, diagnostics, progress, commands, hooks, resources, bulk, media, reverse services and dynamic/UI/lifecycle APIs are not exposed by the current flat text-only ABI. Qualify ownership, exceptions and cleanup at the actual ABI boundary. |
| All languages | Missing/null/default semantics, schema subsets, provisional/active/retired refs, exclusive operation pins, disposer failure, quotas, cancellation versus execution settlement, joint resource/blob admission and durable recovery. Complete the conformance A–E and race/restart/platform matrices without reclassifying skips as passes. |

## Original-extension and deployment qualification backlog

The ten inventory packages are `pi-agent-extensions@0.5.4`,
`pi-subagents@0.59.0`, `pi-background-tasks@2.4.2`, `pi-zentui@0.21.0`,
`@llblab/pi-telegram@0.39.5`, `pi-claude-bridge@0.7.0`,
`pi-langfuse@1.5.15`, `@tintinweb/pi-subagents@0.19.0`,
`@termdraw/pi@0.4.1` and `@lolipopshock/pi-clm@1.0.0`.
`pi-background-tasks` declares peer ranges excluding pinned Pi 1.0.0: resolve
that profile explicitly, without modifying the original or silently widening it.

- **Q01 — scenario completeness:** turn each package's public/private/CLI paths
  into unchanged-source acceptance scenarios, including negative cases and
  combinations. Registration capture does not qualify its runtime behavior.
- **Q02 — production execution:** use actual ExtensionProcess/App/frontend and
  isolated reviewed environments. Preserve original hashes. Native CLM settings
  and termDRAW drawing/save are passes, not blanket acceptance of their packages.
- **Q03 — frontend/platform/endurance:** current-source editor burst and broader
  Doom/footer/drawing scenarios; native input, focus, resize, rescue, crash and
  terminal modes; relevant macOS/Linux/Windows/headless/Serve profiles; sustained
  workloads, backpressure, restart and resource leak checks.
- **Q04 — providers/external services:** real preparation/headers/response/stream,
  tool/media/reasoning round trips, failures, authentication and cancellation.
  Live-provider or outward-facing original-package work requires authorization;
  offline deterministic evidence and live qualification must remain distinct.
- **Q05 — installed release:** separately qualify built/packaged/installed host,
  adapter, dependency graph and activation/trust flow. This is not necessary to
  begin abstract proofs, but is necessary to extend a claim to shipped artifacts.
  The protected installed adapter has not been replaced.
- **Q06 — evidence portability:** preserve sanitized corpus metadata, source
  snapshots, original hashes, harnesses, binary/dependency identities, commands,
  environment and test-selection counts in reproducible evidence storage.
  Current receipts and corpus AST live under ignored `artifacts/`; the checker
  conditionally loads that AST and defaults to a developer-local Pi checkout and
  installed parser. A clean clone cannot yet reproduce this complete inventory.
- **Q07 — mandatory gates:** add inventory/source-drift, adapter, original/native
  acceptance and eventual proof gates to CI. Existing canonical 0.3 conformance
  and Rust workspace tests do not close API 0.4/Pi qualification. Required
  original tests must fail on missing prerequisites or zero selection rather
  than succeed through optional skips.
- **Q08 — reconcile documentation:** older preview/adapter prose describes
  earlier missing startup snapshots, media restrictions and unrun builds.
  Reconcile against exact newer receipts without promoting unqualified editor,
  provider or full-corpus behavior. Retain red/aborted/unstable/zero-test evidence.

The [takeover status](takeover-480-status.md) records existing passing counts.
Receipts qualify only their own stable snapshots; earlier full Rust suites
precede final JavaScript close-ordering edits. A documentation commit does not
convert those receipts into qualification of every later tree or installed build.

## Lean-specific work still required

### L01 — semantic property ledger

Convert the structural inventory into deduplicated behavioral requirements.
Each requirement needs: stable ID; pinned Pi references; observations and input
domain; success/refusal/error/cancellation behavior; feature/frontend/profile;
environment assumptions; implementation paths; native/original scenario IDs;
Lean definitions/theorems; refinement link; evidence hashes; current status.
Map every structural row to requirements, a justified type-only/alias mapping,
or an explicit unresolved obligation. Do not manufacture one theorem per AST row.

### L02 — executable state models and contracts

Formalize at least these property families:

| Family | Required invariants and progress obligations |
|---|---|
| Loading/negotiation | Discovery executes nothing; reviewed activation; exact feature/version/limit agreement; unavailable consumers cannot report success; transitive import trust is explicit. |
| Wire/serialization | Bounded UTF-8 JSONL, wire-specific numbers/schemas, unique/correlated IDs, malformed/duplicate/late replies, atomic frames, bounded queues and writer admission versus completion. |
| Authority | Session/instance/generation ownership, live-parent versus retained authority, revocation and foreground revalidation at mutation; A→B→A cannot revive old grants. |
| Cancellation/execution | At most one caller terminal; caller cancellation, execution settlement, provisional-result disposition and cleanup are distinct; no ambiguous replay; bounded escalation under stated OS assumptions. |
| Callbacks/catalogs | Ordered awaited transforms, promised object identity, synchronous/async contracts, snapshot freshness, transactional descriptors/handlers, collision and stale-call semantics. |
| Sessions | Owner-filtered branch/head/entry identity; single-use append grants and expected-head checks; durable receipt before synchronous append returns; unknown outcomes never replay; compaction cancellation is not rollback. |
| Providers/media | Correct preparation/encoded request/header/response boundaries; host-owned route/auth; ordered stream/terminal semantics; lossless promised projections and authorized artifacts without secret leakage. |
| UI/editor | Host terminal/focus authority; safe bounded frames; mount/owner/geometry/revision fences; latest-frame versus control lanes; close ACK before continuation; checkpoint ACK before matching frame; immediate rescue and stale-write refusal. |
| Resources/themes/buses | Ordered bounded contributions, admission/precedence/retirement, specified color conversion and text widths, distinct local-object and host-bus identity/order/drop semantics. |
| Children/composition | Product admission, ancestry, inherited scopes/budgets, independent reverse-call progress, cancellation/cleanup, persistence and deadlock/cycle prevention. |
| SDK values/resources | Cross-language codec subsets, missing/null/defaults, diagnostics, provisional→active/retired refs, atomic publication, exclusive all-input pins, disposer failure and restart invalidation. |
| Bulk/storage | Single-use tickets, immutable verified snapshots, digest/length/metadata, joint blob/ref result admission, owner/read leases, locator confinement/symlink defenses, quotas, I/O failures and durable recovery. |

Model the real interleavings: factory thread, transport worker, host reader/writer,
pending waiter and independent execution record, App consumer, session-leaf
consumer, provider stream, filesystem and UI mutation. Include queued/writing/
written/skipped frames, timeouts, cancellation, retirement and process death.
Treating an entire reverse request as one atomic operation would hide the races
these repairs addressed.

### L03 — safety, liveness and observable compatibility proofs

Prove inductive safety invariants and identify actual linearization points.
State fairness, consumer scheduling, clock, I/O and termination assumptions for
liveness. Arbitrary trusted CPU-bound JavaScript need not cooperate; a useful
progress theorem permits refusal, cancellation or unknown outcome rather than
promising success. Keep float/color tolerances, Unicode widths, integer ranges,
serialization and bounds explicit rather than replacing them with ideal values.

Specify Pi and Octet observable traces and the permitted relation between them.
Prove simulation/refinement or equivalence for the approved profile, including
errors and asynchronous ordering. A proof of owner safety alone is not a proof
of Pi behavior, and a proof excluding all refused operations cannot establish the
original full target. Safety kernels can be proved before all APIs are implemented.

### L04 — checked linkage to production code

Choose a defensible implementation connection: e.g. a verified/generated
transition core or checked translation with refinement proofs. Account for Rust
mutex/atomic ordering, error/drop/teardown paths, JavaScript workers/promises and
FFI/serialization boundaries. Bind the checked link to exact source identities.
Manual source mappings, extracted traces and differential tests are valuable but
remain evidence, not a mechanized proof of arbitrary Rust/JavaScript executables.
No such checked linkage exists in this candidate.

### L05 — trusted computing base and assumptions

Publish the trust boundary: Lean kernel and admitted axioms; any extraction,
translation or code generator; Rust/JS/C/C++ compilers and runtimes; dependency
versions; OS processes/scheduling/filesystems; terminal behavior; cryptographic
hashes and external providers. State which components are verified versus assumed.
Process isolation and capability declarations are not an OS sandbox. Host-only
proofs cannot forbid trusted extensions from making direct OS calls.

### L06 — reproducible proof project and CI

Add pinned Lean/Lake dependencies and reproducible proof builds after toolchain
installation is authorized. Require theorem/axiom manifests, no `sorry` or
unapproved axioms/unchecked shortcuts, requirement coverage, and source/spec/proof
drift checks. Distinguish specification completeness from theorem compilation.
Keep mandatory native/original/race suites alongside proofs; reject missing
prerequisites, stale receipts and zero-test selections. Bind release-level claims
to the tested/proved packaged artifacts, not merely a similarly named version.

## Recommended execution order and exit criteria

1. **Freeze scope and evidence:** resolve version/profile decisions and make the
   corpus/receipts reproducible (C01–C02, Q06, L01). Keep every unknown visible.
2. **Start proofs of existing safety seams in parallel:** ownership/revocation,
   append-grant linearity, close/composer ordering, editor checkpoints,
   cancellation versus settlement, resource pins and bulk admission (L02–L05).
3. **Close compatibility foundations:** missing lifecycle/public facades,
   events/options/registration, messaging/projections and CLM; then remaining
   render/completion/resource/provider and child/CLI/private paths (C03–C21).
4. **Complete multilingual reachability and actual acceptance:** C22–C23 and
   Q01–Q04; run unchanged originals against stable production snapshots.
5. **Make claims enforceable:** mandatory evidence/proof CI, zero unresolved
   obligations for the approved profile, checked implementation refinement and
   independently reviewed assumptions (Q07–Q08, L06).
6. **Qualify distribution separately:** Q05, only with installation/release
   authorization. No proof or test count here authorizes deployment.

Full completion means every approved requirement has an implemented reachable
behavior, appropriate native/original acceptance, a proved formal obligation and
a checked implementation link, with environmental/TCB assumptions disclosed.
Unchecked implementation linkage remains a gap, not a full-verification claim.
Unresolved requirements remain blockers; shrinking the target is a scope change,
not verification. Lean can begin verifying selected invariants now, but **Lean
cannot yet substantiate full Pi-extension compatibility in Octet**.
