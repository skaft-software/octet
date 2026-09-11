<a id="octet-serve-current-state-and-handoff"></a>

# Serve implementation reference

Maintainer reference for experimental Serve. For installation and practical local
use, read the [Serve guide](README.md). Feature statements below describe the
octet 0.7.5 source implementation; they do not replace per-feature acceptance
evidence. See the [0.7.5 source notes](../../releases/v0.7.5.md); availability,
signed assets, and public-install results belong to the
[exact GitHub release](https://github.com/skaft-software/octet/releases/tag/v0.7.5).

The [validation record](#validation-evidence) and
[historical checklist](p0-p1-delivery.md) retain their original Ygg-era scope.
They are not a pass or waiver of current release gates.
Current work tracking is on the [Project](https://github.com/orgs/skaft-software/projects/5).

## Bottom line

Serve is a real-session experimental local web interface.
[0.7.4](../../releases/v0.7.4.md) is published with signed Serve packages and
verified public installation; its tag and assets are immutable. Those checks
do not qualify the 0.7.5 source. Live-provider/native-host audio checks are
optional and **NOT RUN** in this source review. Signed-package and public-install
checks are separate from actual-terminal/SSH, live-provider/audio, endurance,
and complete graphical media, recovery, or capture acceptance. Private-LAN
pairing and native graphical applications remain specification-only.

## Current status at a glance

This table classifies the supplied implementation description, not independently
observed current behavior.

| Area | Snapshot coverage |
| --- | --- |
| Real local octet agent sessions | Implemented in the described snapshot |
| Streaming, tools, approvals, stop, steer, follow-up | Implemented |
| Multiple independent sessions | Implemented |
| Exception-driven command center | Aggregate needs-you, working, review, complete, and evidence-backed pull-request state; prioritized task queue; task/project search; focused-task handoff |
| Reconnect, replay, resume, and branch checkout | Implemented; current recovery qualification deferred |
| Attachments and prompt documents | PNG/JPEG/GIF/WebP images plus bounded text, Markdown, and PDF context; no audio; media qualification deferred |
| Sources, diffs, and outputs | Real but limited to specific built-in tools |
| Live generated-site previews | Fixture UI exists; production capability is off |
| Projects and folder management | Durable private multi-project registry, trust/default/archive/session binding, repository context, and trusted file browsing; no host-native folder picker |
| Session retention | Archive, recoverable trash/restore, guarded permanent deletion, and startup recovery contracts |
| Context and compaction telemetry | Authoritative replayable accounting in the agent, protocol, runtime inspector, and composer |
| Integrated terminal | Production PTY panel when session process execution is allowed |
| Extension, skills, MCP, or LSP GUI | Composer discovery supports trusted skills, prompt templates, and enabled extension commands; no dedicated management GUI, MCP, or LSP |
| Child-agent visualization | Not implemented; the runtime does not expose it |
| LAN-connected devices | Designed, not implemented |
| macOS, iOS, and Android applications | Designed; no app projects or signed builds |
| Coding-workbench layout | Described across desktop, tablet, and phone; new captures deferred |

<a id="settled-product"></a>

## Task model

- `octet serve` runs octet headlessly.
- There is one repository-oriented task lifecycle with two complementary views:
  an exception-driven command center for supervision and a focused transcript
  for execution.
- Opening the root creates a fresh provisional task. `/overview` opens the
  command center from session inventory without creating or opening a task, and
  an explicit session route restores a focused task.
- Previous, pinned, and running tasks appear in the sidebar.
- Different tasks are independent agent sessions and may run concurrently.
- There is no Chat, Code, Work, or Cowork mode selector.
- There is no synchronized TUI, terminal mirror, or terminal-window mode.
- The proposed phone client is a companion/controller for the host, not a mobile
  agent runtime.
- The command center derives aggregate state, ordering, search text, and row
  previews from the existing session catalog. It does not create a second
  orchestration protocol or synthetic agent narrative.
- The transcript is primary within focused work. Sources, actions, outputs,
  previews, and progress appear only when structured events justify them.
- There is no account, login, hosted control plane, or outbound product
  telemetry. Local context, lifecycle, and usage accounting remain part of the
  workbench state.
- octet retains broad local authority by default. Network authentication remains
  separate from agent authority.
- The interface is a deterministic projection of real octet events. It does not
  add UI instructions to the model or ask another model to invent summaries or
  cards.

<a id="settled-visual-and-interaction-language"></a>

## Visual and interaction reference

The source-described visual contract is:

- Always spell the product `octet`; keep the wordmark unboxed and unaccompanied
  by a decorative logo in the sidebar header.
- Use one opaque, neutral-dark workbench appearance. Do not use glass,
  translucency, or model-driven application colors. Rainbow color is reserved
  for the reasoning-effort control.
- The focused desktop shell is a real three-pane workbench: a 296px project/task
  sidebar, a broad center pane, and an optional 400px evidence pane. The command
  center uses the same sidebar with one broad supervisory surface. Distinct
  shaded surfaces separate panes without one-pixel divider lines.
- Label navigation as Tasks, retain project grouping, and keep each sidebar row
  to its title plus an optional PR mark. The PR mark appears only for structured
  `in_progress`, `ready`, or `merged` evidence; repository state, model metadata,
  and generic run status are not synthesized into the row.
- Keep the command center exception-driven: semantic status totals lead to one
  compact, aligned queue rather than a kanban board or a wall of agent cards.
  Failed, disconnected, attention-required, and review-ready work sorts ahead of
  healthy running or completed work.
- Status color is semantic: green means working or successful, amber means
  attention, and red means failure. Provider and model colors do not determine
  shell state.
- Keep the fresh task quiet: “New workspace task,” “What should we work on?”,
  and a short instruction replace the animated TUI/model splash.
- Compose the transcript like an engineering record, not a chat product. User
  turns and expanded tool calls use restrained tonal surfaces without thin card
  outlines; assistant prose and grouped tool evidence remain primary. Prior
  actions collapse into one concise activity summary while the latest streaming
  item stays visible on its own line. Reasoning, commands, metadata, output, and
  completion evidence remain available through disclosures.
- The composer spans the usable center pane, has restrained geometry, and stays
  visually attached to the work surface. It has no border, shimmer, perimeter
  chase, glass, or model-colored chrome, while focus remains visibly ringed.
- Use Local Grotesk and Local Mono from bundled Local Type System 0.53 by
  default. Their compact variable web fonts cover the full 400–700 interface
  range without separate weight files. Open counters, differentiated ambiguity
  forms, restrained lower-half gravity, and a shared authored construction
  grammar balance legibility with a playful DIY character without making a
  clinical accessibility claim. The visible type scale has two sizes: 14px
  interface text and 12px metadata. Monospace remains limited to code, paths,
  diffs, commands, and technical metadata. Popular device-installed font
  pairings and size preferences remain available.
- The model picker keeps the simple model/effort abstraction, with precise
  controls under Advanced. Ordinary effort fills the track in blue through and
  slightly behind the white thumb without a rounded-cap gap. Exact `xhigh` adds
  varied white particles that float locally within the blue fill. Exact `max`
  combines those particles with the animated rainbow and is the only rainbow
  state. Reduced motion freezes that rainbow and removes all particles. Changing
  models never changes the shell appearance.
- Do not duplicate Working or Activity indicators inside focused-task chrome.
  The optional right pane is limited to review, command history, progress,
  artifacts, and context backed by structured events.
- A dominant inspector opens source, output, image, and diff content without
  changing transport or transcript behavior.
- Mobile displays one primary surface at a time. Navigation, Activity, and the
  inspector become full-height overlays while the composer keeps all controls
  keyboard accessible.
- Connected Devices appears only when the host advertises that capability; do
  not invent account, machine, connection, or evidence state.

The typography policy and browser baselines cover full-shell desktop,
performance transcript, mobile completion review, and mobile inspector states.
The original source description calls this an original React/CSS implementation,
with no proprietary vendor code, bundle, or asset copied. That attribution is
retained here, not presented as a new provenance audit.

## Architecture

The described shape is:

```text
React 19 web client
  -> versioned octet-serve protocol and deterministic reducer
    -> loopback Rust host/service
      -> session supervisor
        -> one serialized actor per graphical session
          -> feature-gated octet-coding-agent adapter
            -> one private App/Agent/session owner
```

See [architecture](architecture.md) for package, protocol, and adapter boundaries.
The optional backend is deliberately excluded from the ordinary Cargo workspace.
The `serve` feature is disabled by default, so the normal TUI and agent do not
depend on the web surface. The default binary can install and launch a separately
packaged feature-enabled runtime.

The snapshot's package and launch CLI is:

```text
octet extension install octet-serve
octet extension install --path <archive>
octet extension list
octet extension update octet-serve
octet extension remove octet-serve
octet serve
  --no-open
  --port <u16>
  --web-root <directory>
```

Catalog installation and update select the package matching the running octet
version exactly; the 0.7.5 commands require matching assets on its GitHub release.
A reviewed matching local archive or direct source build does not require public
package availability. There is no implemented `--lan`, `--demo`, or `--local-only`
switch. The shortest direct source launch is:

```console
cargo run --features serve -- serve
```

The feature is needed when building the packaged runtime from source; an ordinary
installation dispatches `octet serve` to that runtime. `--port 0` requests an
ephemeral port. See [package usage](README.md#install-or-update-a-package).

Configuration loading reports unknown global and trusted-project TOML keys with
source path, line, column, dotted key, and a bounded typo suggestion. Unknown
keys warn by default for compatibility. The global `--strict-config` flag,
`strict_config = true`, or `OCTET_STRICT_CONFIG=true` makes the collected
unknown-key diagnostics fatal. Known compatibility aliases remain accepted.
See [Configuration diagnostics](../../design/config-diagnostics.md).

<a id="what-is-genuinely-implemented"></a>

## Implementation coverage

All statements in this section describe the supplied experimental snapshot.
They do not replace final-source review or current-version acceptance.

### Real host and sessions

- A feature-gated `octet serve` binary path.
- An IPv4 loopback-only Axum host.
- An embedded frontend with asset digests.
- A one-use launch capability exchanged for an ephemeral HttpOnly,
  SameSite=Strict cookie.
- Strict Host, Origin, and Fetch Metadata validation.
- No CORS or remote assets.
- Bounded HTTP, WebSocket, replay, resource, and attachment payloads.
- Fresh-session root behavior.
- Explicit session restoration and a durable, inventory-only `/overview`
  command-center route.
- Exception-prioritized active-task aggregation and task/project search using
  existing session summaries.
- Concurrent independent graphical sessions.
- Exactly one mutable `App` owner per session.
- A durable, owner-private project registry with opaque IDs, root-identity
  revalidation, explicit trust, defaults, archive, and session bindings.
- Session rename and pin plus active/archive/trash lifecycle views.
- Restore from trash and exact-phrase permanent deletion through a durable,
  crash-recoverable cleanup journal.
- Host-authoritative session titles and catalog updates.
- Authenticated bounded transcript search and redacted JSON export.
- Branch graph projection and safe idle-boundary branch checkout.

### Lifecycle and persistence safety

- Git probes and PTY shells own Unix process groups and use bounded graceful/
  forced descendant cleanup; retained output descriptors cannot hang shutdown.
- PTY output uses incremental UTF-8 decoding and valid-boundary replay
  truncation.
- Trusted project reads and writes use root-identity revalidation,
  descriptor-relative no-follow traversal, conflict checks, atomic replacement,
  content synchronization, and owning-directory synchronization.
- WebSocket connections and store initialization are generation-scoped, so
  stale callbacks, replay responses, and timers cannot replace newer state.
- Permanent deletion journals intent before the transcript boundary, rolls back
  interrupted pre-commit work, and retries committed cleanup idempotently after
  restart. It removes session-owned attachments, documents, resources, run
  records, goals, project bindings, and search data while retaining shared
  payloads and conversation-content-free inference accounting.
- Missing required stores fail permanent deletion before commit rather than
  producing a partially deleted session.

See [Serve lifecycle and safety](../../design/serve-lifecycle-safety.md) for the
full trust and recovery contracts, including the limits of process cleanup.

### Agent interaction

- Real prompt submission through the coding-agent path.
- Streaming assistant text and reasoning.
- Structured tool calls, results, and progress.
- Approval and typed-input requests.
- Stop, steering, and queued follow-up.
- Prior-turn edit, response retry (including model override), conversation
  fork, and whole-session fork at idle durable boundaries.
- Durable run outcomes.
- Authoritative context categories, response/tool lifecycle counters, and
  compaction start/finish/failure projections.
- Model catalog, model selection, and reasoning-effort selection.
- Composer `@` completion backed by trusted project-file IDs; selected files remain
  explicit context instead of being silently injected into user text.
- Session-scoped `/` discovery and typed idle-boundary invocation for built-in
  commands, prompt templates, host-admitted skills, and enabled extension
  commands.

Context and run lifecycle state is derived from the active agent run and
published as replayable full-state `context.updated` replacements. Polling does
not mutate durable conversation history. Every started provider response is
reconciled as finished, discarded, or active, and adapter source attribution is
added only where authoritative metadata exists; unmatched provider totals stay
in `other`. Legacy `usage.updated` events remain accepted.

The production authority catalog is derived from host sandbox configuration:
`ReadOnly` is always listed, `Workspace` is added when writes are allowed, and
`FullAccess` when process execution is allowed. These are not independently
enforced per-session sandboxes: `SetAuthority` changes the session setting,
not the worker's tool/process configuration. Do not rely on choosing `ReadOnly`
or `Workspace` to restrict an otherwise full-access host. Configure host policy
before launch and use OS isolation for hostile work.

### Attachments and prompt documents

- PNG, JPEG, GIF, and WebP image attachments.
- MIME sniffing and byte limits.
- Paste, drop, picker, and attachment-only submission.
- Private persistent attachment storage with count/byte reservations that remain
  correct under concurrent ingest.
- Thumbnails and authenticated retrieval.
- Native image input for models that support it.
- Bounded UTF-8 text, Markdown, and ordinary PDF document ingest with immutable
  extraction provenance, hostile-input limits, private storage, and explicit
  prompt-context selection.
- Immutable trusted project-file snapshots selected by opaque file ID.

Audio and other media attachment types are not implemented by the production
host. Native-host audio acceptance is a separate interface, not web support.

### Sources, changes, and outputs

The snapshot describes a durable evidence store in
`extensions/octet-serve/src/resource.rs`:

- immutable evidence blobs;
- versioned metadata and binding records;
- commit manifests;
- SHA-256 integrity verification;
- restart recovery;
- session scoping;
- quotas and bounded reads;
- symlink and path defenses;
- rollback for partial commits;
- exact source content;
- actual unified diffs;
- post-change file snapshots; and
- intentional artifact promotion for newly created Site, Document,
  Spreadsheet, and Presentation outputs.

Coverage is deliberately narrow. Deterministic evidence comes from successful
built-in `read`, `read_skill_resource`, `edit`, and `write` operations. General
Bash or extension mutations, delete and rename, binary changes, web provenance,
and arbitrary tool ecosystems are not captured comprehensively.

### Frontend

- Project-grouped Tasks sidebar with title-only rows and evidence-gated PR
  marks, plus task-title/preview and transcript search.
- Exception-driven command center with aggregate status, search, priority triage,
  focused-task handoff, and a durable `/overview` route.
- One opaque neutral workbench visual system with semantic status colors and
  borderless tonal pane separation.
- Transcript with Markdown, GitHub-Flavored Markdown, concise collapsed work
  summaries, a separate live item, and expandable typed detail.
- Source, diff, output, and image openers.
- Approval and input interactions.
- Compact run outcome, changed-file, and elapsed-time presentation.
- Pane-width composer with attachments, context, model, effort, authority,
  follow-up/steer, and send/stop.
- Resizable desktop Activity and Inspector panes with full-surface mobile
  overlays.
- Bundled Local Grotesk and Local Mono typography by default with a two-size
  interface/metadata scale and popular device-local font and size alternatives.
- A blue reasoning slider for ordinary effort, locally floating varied white
  particles for exact `xhigh` and `max`, an animated rainbow reserved for exact
  `max`, and static, particle-free reduced motion.
- Settings, review, command history, progress, artifacts, repository context,
  and authoritative context/compaction inspection.
- Project trust/default/archive management and active/archive/trash task
  navigation with guarded permanent deletion.
- A retained, bounded PTY terminal panel when the host advertises process
  execution authority.
- Responsive desktop, tablet, and phone layouts.

The earlier fixture response:

```text
Request understood
Inspected the project context
```

was not a real agent response. Fixture sessions display an explicit simulated-data
banner, and a production-build assertion prevents fixture transport from becoming
reachable as production behavior. Fixtures are development/test inputs only.

## What remains fixture-only, specified, or absent

### Projects and context

- The project model is a durable private registry, not a synthetic
  launch-workspace row. It supports multiple opaque project IDs, one canonical
  root per project, explicit trust, defaults, archive, and durable session
  bindings.
- The loopback browser cannot mint filesystem authority, so it cannot import an
  arbitrary folder. Launching the host for a workspace registers that real
  root; a future host-native picker must supply one-use opaque candidates.
- There is no multi-root project or project-scoped extension-set model, and an
  archived project has no restore UI yet.
- Authenticated, bounded transcript-content search is implemented alongside
  client-side title/preview filtering.
- There is no tags UI or general session import workflow.
- There is no global command palette; composer `/` discovery is the implemented
  command surface.
- There is no general deep-link system beyond sessions and branches.

### Session semantics

- Edit, retry, conversation fork, and session fork are implemented only at
  validated idle/committed boundaries; there is no filesystem rollback tied to
  conversation checkout.
- Bounded independent text and attachment drafts persist per host/session in
  browser storage and clear only after an acknowledged submission.
- Accepted queued follow-ups are not durable across a host restart and cannot be
  fully edited, removed, or reordered.
- Provider retries are not shown with attempt count, delay, and sanitized
  cause.
- Context and compaction lifecycle is projected live and replayed within a host
  run, but the operational tracker is intentionally not conversation persistence.
- Structured PR state is produced from bounded `gh` JSON after admitted runs,
  persisted in a Serve-owned sidecar, refreshed for hosted and inventory-only
  sessions, and projected through live session/catalog events. Temporary lookup
  failures retain prior valid evidence; authoritative closure removes it.
- Structured plans exist in DTOs and fixtures, but the real adapter does not
  produce them.

### Outputs and previews

- Production advertises `previews: false`.
- The visible site preview in fixture mode is not a registered production
  live-service preview.
- There is no generalized filesystem change watcher.
- There is no artifact library, output version history, rename/delete
  workflow, or generalized preview sandbox.

### Extensions and agent ecosystem

The web composer has limited, session-scoped parity with TUI resource discovery:

- it exposes only skills, prompt templates, and executable extension commands
  already admitted by the host's existing trust policy;
- `/skills` accepts the TUI list, show, active, search, load, reload, and off
  workflow, with selectable skill names from the discovery payload;
- `/reload`, `/skills reload`, and `/extensions reload` rebuild dynamic
  instructions, prompts, skills, and enabled extensions at an idle boundary;
  and
- extension commands that need an interactive extension confirmation are denied,
  because the web host does not expose a confirmation bridge or an extension
  output panel.

The web surface excludes:

- MCP management;
- a plugin or extension catalog and lifecycle UI;
- extension diagnostics and a dedicated extension-output surface;
- LSP;
- scheduling;
- TUI synchronization; and
- child-agent runtime trees.

The production host advertises `terminal: true` only when host configuration
allows process execution, independently of the session authority picker. The
terminal is a bounded retained local PTY whose WebSocket authority is derived
from the authenticated page origin.
`childAgents` remains false; the UI does not fake child-agent state. None of these
package-specific contracts supplies a qualified extension API 0.3 runtime example.

### LAN and native

[LAN pairing](lan-pairing.md) is specified but unimplemented. The design is:

- accountless and Syncthing-like;
- LAN-only for v1;
- explicit pairing with no automatic LAN trust;
- stable host and device identities;
- a human-verifiable QR and fingerprint ceremony;
- TLS 1.3 with a host-local CA;
- a client-pinned host CA;
- per-device 256-bit credentials;
- a revocable trusted-device registry;
- one authoritative host;
- companion clients rather than replicated agents; and
- deferred WAN, rendezvous, NAT traversal, and relays.

Production capability flags remain false for Connected Devices and LAN clients.
The [native design](native-delivery.md) proposes shared React in thin system-webview
shells. Tauri 2 is an unvalidated candidate; Electron is out of scope. There are no
Tauri, Xcode/iOS, or Android projects; Developer ID signing, notarization,
provisioning profiles, TestFlight builds, signed APKs, or Android App Bundles
for these graphical shells. Signed native CLI and Serve runtime archives are
separate release artifacts, not native graphical apps.

## Validation evidence

**Historical Ygg v0.4.0 evidence, not octet 0.7.0 qualification.** Names,
commands, branch identity, and bundle hash below retain their original scope.
The following record is retained, not rerun for this proposal.

The final hardening matrix was run with locked dependencies. Web checks used the
pinned `apps/web/.node-version` runtime, Node `v22.13.0`.

| Gate | Result |
| --- | --- |
| Web install | `npm ci` passed with zero reported vulnerabilities |
| Web lint, typecheck, typography, production build, same-origin/CSP audit, and embedded-bundle check | Pass |
| Web unit tests | Full Vitest suite passed |
| Fixture Playwright matrix | Every applicable test passed for desktop, tablet landscape, tablet portrait, mobile, and mobile-small |
| Production-host Playwright | 1/1 passed against the real Rust host and a disposable local OpenAI-compatible provider; authentication/model selection, streaming, tool replay, `429`/`408` retries, explicit compaction, restart/resume, cancellation, and secret-safe failure projection are covered |
| `ygg-agent` tests | 219 library and 64 agent-run integration tests passed |
| Coding-agent tests | 753 passed with `serve`; 671 passed with default features |
| Full Rust workspace tests | All targets/all features and documentation tests passed |
| No-default-feature workspace check | Pass |
| Independent `extensions/ygg-serve` tests | 115 library tests and every integration suite passed |
| Strict workspace and independent-extension Clippy | Pass |
| Rust 1.86 workspace and independent-extension checks | Pass |
| Rust formatting and `git diff --check` | Pass |
| Package-boundary script | Pass |
| Publishable core workspace package assembly | `cargo +1.90.0 package --workspace --exclude ygg-coding-agent --locked --no-verify` passed from the clean `v0.4.0` candidate tree; Cargo 1.90 is used only for interdependent package assembly, while compilation remains on the Rust 1.86 MSRV |
| Installed `ygg-coding-agent --features serve` smoke | Pass; installed `ygg 0.4.0` served the synchronized embedded bundle |
| Optimized feature-enabled build and bundle smoke | Pass locally with the release binary; no signed tag artifact published |
| Reproducible Serve archive and package dispatch | Two optimized Apple-silicon archives were byte-identical; local install, list, package-dispatched launch, embedded-bundle verification, removal, and data preservation passed |
| Optimized signed serve release | Workflow defined; not yet run against a release tag |

`lopdf` is pinned to `0.42.0`; the independent serve manifest and lockfile are
kept explicit so the PDF parser remains on the audited version. Serve retains
its own strict PDF header, envelope, classic-xref, revision, size, object, and
nesting limits around that parser. A hostile-input regression constructs 4,096
direct nesting levels and verifies Ygg's iterative preflight rejects the input at
its 64-level bound before `lopdf` parsing.

The fixture matrix was split by configured Playwright project after the combined
165-test invocation exceeded the command runner's 120-second limit; no test
failure caused that timeout. Every project then passed independently under the
pinned Node runtime. The strict post-stream 50 ms long-task benchmark also runs
in a separate Playwright invocation to keep unrelated functional cases outside
its application-pressure window.

The production-host Playwright test uses the real Rust host, real session
adapter, and real provider request path with a disposable local
OpenAI-compatible provider. It now covers the configured-provider conformance
scenarios listed above without inheriting external credentials. Credentialed
checks remain separately protected but are temporarily optional for stable
releases; their supported routes, required environment variables, handling
rules, and current `v0.4.0` waiver are recorded in
[configured-provider acceptance](provider-acceptance.md).

The synchronized embedded bundle has SHA-256:

```text
bc411e451925a63ec17926db70d5a9cf1717d3168cee6deeff3751d2420cc59a
```

## Repository checkpoint

Historical identities retained for interpreting the evidence, not current work
instructions:

- Repository: `skaft-software/ygg`.
- Experimental branch: `explore/ygg-serve-web-v2`.
- Pre-hardening branch tip and forward boundary checkpoint:
  `eebe7389097cdcf27cc22b26da75b57a06e4e8e8`.
- The recorded hardening pass was uncommitted; its table does not imply a merge,
  commit, or push.
- Rejected frontend reference: `archive/octet-workbench-rejected-20260726`,
  commit `7ca26e3`, approximately 20,751 added lines across 90 files. It is a
  historical reference, not the current interface.

## Package boundary

The technical boundary keeps the web product in `apps/web`, the optional backend
in `extensions/octet-serve`, and only narrow generic seams in core crates.
The retained boundary evidence uses `eebe738`, not the older `c6ec60f` comparison
that included unrelated stacked core/TUI history. The previously reported seven
violations were stale; broadening the allowlist would not prove isolation.

`scripts/check-octet-serve-boundaries.sh` is described as using `eebe738` for
forward enforcement. It requires the selected base to be an ancestor of `HEAD`
and admits only:

- the application, optional extension, integration adapter, and their
  documentation/build paths;
- generic agent-owned context accounting in
  `crates/octet-agent/src/{agent,context,lib}.rs` and its agent-run tests;
- generic coding-agent configuration diagnostics in `config.rs`,
  `resource_resolver.rs`, and `resources.rs`; and
- the generic primary-session deletion primitive in `session_store.rs`.

The historical default gate passed; an explicit old-base audit failed on
unrelated/historical paths. This forward delta result is not proof that the
entire pre-checkpoint branch history is boundary-clean or qualified against a
final integration target.

## Visual truth

The retained browser inspection record described:

- a 296px project/session sidebar, broad transcript, and optional 400px evidence
  pane;
- neutral opaque headers, user turns, action groups, composer, Activity, and
  Inspector rather than warm tint or structural divider lines;
- title-only rows with evidence-gated green or purple PR marks, not model-colored
  navigation or composer chrome;
- concise action-and-duration completion summaries, collapsed prior live work,
  a visible current live item, and disclosed commands, reasoning, metadata,
  output, and completion review;
- blue ordinary effort, varied local white particles at exact `xhigh` and `max`,
  rainbow only at exact `max`, and static particle-free reduced motion;
- 14px UI and 12px metadata tokens;
- browser inspection of fresh, populated, Activity, Inspector, performance,
  tablet, 390px phone, and 360px phone states; and
- checked-in full desktop, focused performance, completion-review, and
  mobile-inspector baselines.

These are historical inspections, not new captures. Production Activity can be
sparse because plans and previews have limited real producers. Project import
lacks a host-native picker, and fixtures cannot represent every long-running
real-agent shape. The Activity pane remains user-controlled rather than
appearing without structured evidence.

<a id="how-the-effort-evolved"></a>
<a id="recommended-next-sequence"></a>

## Project

- [Project](https://github.com/orgs/skaft-software/projects/5) — current work tracking.
- [Manual Serve acceptance](provider-acceptance.md#manual-serve-acceptance) — retained real-provider journey criteria; qualification remains deferred.
- [LAN specification](lan-pairing.md) and [native design](native-delivery.md) — unimplemented technical references.
