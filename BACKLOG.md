# Octet — complete remaining worklist

Generated from `ROADMAP.md`, all GitHub issues (310 total, 142 open), and
**Project 5 “Octet engineering backlog”** (157 items: 141 open, 16 done).
Every open board item appears exactly once, in its board queue. Issue numbers link
to GitHub; board metadata (track · roadmap state · effort · review gate · type) is shown per line.
Bug = defect fix · Feature = new capability · Verification = qualify released behavior with
evidence (repair only a demonstrated gap) · Refactor = behavior-preserving split · Research = spike.

**Path:** Phase 0 (release gates, 9) → Phase 1 (next, 11) → Phase 2 (accepted+blocked, 25) →
Phase 3 (acceptance audits, 20) → Phase 4 (later, 58) → epics (17 umbrellas) → external (3) → orphans (1) → not promised → done.

---

## Captured 2026-09-15 — subagent fan-out (found in a live session, not on the board)

- [ ] **subagents: a worker must survive the parent turn** — today the host retires children with the owning run (`crates/octet-agent/src/delegation.rs`, `DelegatedAgentStatus::Shutdown` → *"worker was shut down by its owning run"*; the extension's `REFERENCE.md:219` documents the resulting `orphaned` rows). A spawned worker therefore dies the moment the parent turn ends, which makes any fan-out longer than a single turn impossible — the extension looks broken to anyone using it that way. Needs a session-scoped delegation lifetime: durable child records, reattachment by the owning session on a later turn, an explicit parent wait, and a documented answer for **unattended mutation** (children inherit the sandbox/approval authority, so "background" means tool use continues after the user's turn has ended). Security review.
- [ ] **subagents: per-worker model selection** — `extensions/octet-subagents/octet_subagents/model.py:191-194` rejects any value but `"inherit"` ("API 0.2 agent_sessions can only inherit the parent model") and `runtime.py:57-59` hard-codes the enum in the advertised schema; the host bakes the parent model into every child (`delegation.rs` builds the child with `model: self.template.model.clone()`). Allow **any model the user has configured**, with `inherit` as the recommended default. Keep pricing/cost-ceiling validation (`delegation.rs:783` already requires trusted pricing when a child carries a cost ceiling). Motivation: cheap/fast child models are what make fan-out affordable — one 8-worker batch today cost ~3.6M input tokens on the parent's model for zero delivered rows.

---

## Phase 0 — release gates (Queue: Now, 9)

Roadmap “Now”: verify install/TUI/`/model`/resumable sessions (#354), dependable media (#379),
reconnect + responsive API waits + stable Browse focus (#350, #346, #377), runnable API 0.3
authoring path (#253). Most items are verification/evidence on released builds, not rewrites.

- [ ] **[#193](https://github.com/skaft-software/octet/issues/193) Docs: qualify current install, media, extension and roadmap journeys** — Evidence & launch · Acceptance audit · documentation
- [ ] **[#253](https://github.com/skaft-software/octet/issues/253) Extensions: qualify the API 0.3 contract and a runnable authoring example** — Daily driver · Acceptance audit · enhancement
- [ ] **[#346](https://github.com/skaft-software/octet/issues/346) Stability: qualify responsive TUI input and redraw during API waits** — Daily driver · Acceptance audit · effort M · Independent review · bug
- [ ] **[#350](https://github.com/skaft-software/octet/issues/350) Stability: qualify Codex reconnect recovery on released builds** — Daily driver · Acceptance audit · effort M · Security review · bug
- [ ] **[#354](https://github.com/skaft-software/octet/issues/354) Next release: qualify core workflows, media and extension authoring** — Daily driver · Accepted / Next · effort Epic · Human decision · type/epic
- [ ] **[#377](https://github.com/skaft-software/octet/issues/377) bug: octet-browse window flickers and steals focus during browser tool use** — Daily driver · Accepted / Next · bug
- [ ] **[#379](https://github.com/skaft-software/octet/issues/379) Media: reliable audio/image attachments and explicit unsupported-input errors** — Daily driver · Accepted / Next · Security review · bug
- [ ] **[#428](https://github.com/skaft-software/octet/issues/428) bug(web-search): make SearXNG and Brave deadlines consistent through the tool runtime** — Daily driver · Accepted / Next · bug
- [ ] **[#429](https://github.com/skaft-software/octet/issues/429) bug(tui): Enter should invoke the selected slash command, not only complete it** — Daily driver · Accepted / Next · bug

---

## Phase 1 — next (Queue: Next, 11)

- [ ] IN PROGRESS — **[#179](https://github.com/skaft-software/octet/issues/179) Feature: support remote Streamable HTTP MCP servers in octet-mcp** — Pi & runtime · Accepted / Next · Security review · enhancement
- [ ] **[#276](https://github.com/skaft-software/octet/issues/276) sexy-tui-rs: add bounded Kitty/iTerm2 image protocol foundation** — Daily driver · Accepted / Next · enhancement
- [ ] **[#313](https://github.com/skaft-software/octet/issues/313) Maintainability: isolate configuration diagnostics without changing behavior** — Qualification & repair · Accepted / Next · enhancement
- [ ] **[#349](https://github.com/skaft-software/octet/issues/349) tui: live steering, Escape queued dispatch, and Option+Up queue editing** — Daily driver · Accepted / Next · effort L · Independent review · enhancement
- [ ] **[#378](https://github.com/skaft-software/octet/issues/378) browser: operate explicitly selected existing tabs/windows, especially Firefox and Safari** — Computer use · Accepted / Next · Security review · enhancement
- [ ] **[#381](https://github.com/skaft-software/octet/issues/381) feat(tui): browse sent prompt history with Up/Down at composer boundaries** — Daily driver · Accepted / Next · enhancement
- [ ] **[#383](https://github.com/skaft-software/octet/issues/383) automation: authorize scoped browser/desktop actions through host policy** — Computer use · Accepted / Next · Security review · enhancement
- [ ] **[#387](https://github.com/skaft-software/octet/issues/387) sessions/media: retain bounded automation screenshots by reference and project only selected frames** — Computer use · Accepted / Next · Security review · enhancement
- [ ] **[#392](https://github.com/skaft-software/octet/issues/392) tui: preserve terminal-owned scrollback during active work** — Daily driver · Accepted / Next · Independent review · bug
- [ ] **[#393](https://github.com/skaft-software/octet/issues/393) tui: keep completed Markdown stable while thinking and text stream** — Daily driver · Accepted / Next · Independent review · enhancement
- [ ] **[#424](https://github.com/skaft-software/octet/issues/424) provider(discovery): standardize capability self-description so unchanged builds pick up new models** — Pi & runtime · Accepted / Next · enhancement

---

## Phase 2 — accepted but blocked on prerequisites (Queue: Blocked, 25)

Accepted scope queued behind epics: Pi provider runtime (#2), Pi compatibility (#190),
computer use (#345), TUI image pipeline (#153), extension runtime (#190/#163).

### 2a. Pi provider implementations + compatibility (6)

- [ ] **[#245](https://github.com/skaft-software/octet/issues/245) provider: implement Mistral Conversations** — Pi & runtime · Accepted / Next · enhancement
- [ ] **[#248](https://github.com/skaft-software/octet/issues/248) provider: implement Google Vertex inference and ADC resolution** — Pi & runtime · Accepted / Next · enhancement
- [ ] **[#249](https://github.com/skaft-software/octet/issues/249) provider: implement GitHub Copilot OAuth, token refresh, and protocol routing** — Pi & runtime · Accepted / Next · enhancement
- [ ] **[#250](https://github.com/skaft-software/octet/issues/250) provider: implement Cloudflare Workers AI** — Pi & runtime · Accepted / Next · enhancement
- [ ] **[#252](https://github.com/skaft-software/octet/issues/252) provider: close the remaining pinned Pi provider compatibility inventory** — Pi & runtime · Accepted / Next · enhancement
- [ ] **[#271](https://github.com/skaft-software/octet/issues/271) extensions: broker provider OAuth and credentials through host-owned policy** — Pi & runtime · Accepted / Next · enhancement

### 2b. Pi compatibility (5)

- [ ] **[#258](https://github.com/skaft-software/octet/issues/258) pi-compat: pass plan mode, all 78 examples, and the complete public-surface ledger** — Pi & runtime · Accepted / Next · enhancement
- [ ] **[#259](https://github.com/skaft-software/octet/issues/259) pi-compat: add bounded semantic UI contributions and renderer transport** — Pi & runtime · Accepted / Next · enhancement
- [ ] **[#260](https://github.com/skaft-software/octet/issues/260) pi-compat: add bounded editor, focus, input, resize, and autocomplete handoff** — Pi & runtime · Accepted / Next · enhancement
- [ ] **[#272](https://github.com/skaft-software/octet/issues/272) pi-compat: bridge registerProvider, provider hooks, streaming, and OAuth** — Pi & runtime · Accepted / Next · enhancement
- [ ] **[#397](https://github.com/skaft-software/octet/issues/397) pi-compat: install unchanged Pi packages and their dependencies safely** — Pi & runtime · Accepted / Next · Security review · enhancement

### 2c. Computer use (7)

- [ ] **[#384](https://github.com/skaft-software/octet/issues/384) browser: integrate a pinned upstream Playwright backend with opt-in existing Chrome/Edge tabs** — Computer use · Accepted / Next · Security review · enhancement
- [ ] **[#385](https://github.com/skaft-software/octet/issues/385) computer use: integrate and qualify a macOS native automation backend** — Computer use · Accepted / Next · Security review · enhancement
- [ ] **[#386](https://github.com/skaft-software/octet/issues/386) computer use: complete persistent observation/action lifecycle and trusted stop/takeover** — Computer use · Accepted / Next · Security review · enhancement
- [ ] **[#388](https://github.com/skaft-software/octet/issues/388) providers/computer use: preserve public OpenAI Responses computer_call and output lifecycle** — Computer use · Accepted / Next · Security review · enhancement
- [ ] **[#389](https://github.com/skaft-software/octet/issues/389) computer use: retain Windows backend, identity, permission and security qualification** — Computer use · Accepted / Next · Security review · enhancement
- [ ] **[#390](https://github.com/skaft-software/octet/issues/390) computer use: package and qualify full parity in one release** — Computer use · Accepted / Next · Security review · enhancement
- [ ] **[#391](https://github.com/skaft-software/octet/issues/391) computer use: contain the persistent model-code automation runtime** — Computer use · Accepted / Next · Security review · enhancement

### 2d. TUI image pipeline (2)

- [ ] **[#277](https://github.com/skaft-software/octet/issues/277) tui: retain bounded tool-result image media through live state and resume** — Qualification & repair · Accepted / Next · enhancement
- [ ] **[#278](https://github.com/skaft-software/octet/issues/278) tui: render inline tool images with settings, selection, and scrollback safety** — Daily driver · Accepted / Next · enhancement

### 2e. Extension runtime surface (5)

- [ ] **[#41](https://github.com/skaft-software/octet/issues/41) Keyboard shortcut registration for extensions** — Pi & runtime · Accepted / Next · enhancement
- [ ] **[#47](https://github.com/skaft-software/octet/issues/47) CLI flag registration for extensions** — Pi & runtime · Accepted / Next · enhancement
- [ ] **[#254](https://github.com/skaft-software/octet/issues/254) extensions: add host/workspace runtime manager and App session bindings** — Pi & runtime · Accepted / Next · enhancement
- [ ] **[#255](https://github.com/skaft-software/octet/issues/255) extensions: enforce aggregate process, FD, byte, startup, and restart governance** — Pi & runtime · Accepted / Next · enhancement
- [ ] **[#267](https://github.com/skaft-software/octet/issues/267) extensions: add typed PostMutation rescan hook** — Pi & runtime · Accepted / Next · enhancement

---

## Phase 3 — acceptance audits (Queue: Audit, 20)

Qualify already-implemented behavior: reproduce on a released candidate, record evidence,
repair only a demonstrated gap.

### 3a. Providers (2)

- [ ] **[#244](https://github.com/skaft-software/octet/issues/244) provider: implement Google Generative AI and Gemini discovery** — Pi & runtime · Acceptance audit · enhancement
- [ ] **[#246](https://github.com/skaft-software/octet/issues/246) provider: implement Amazon Bedrock Converse with AWS credential resolution** — Pi & runtime · Acceptance audit · enhancement

### 3b. Extensions API (9)

- [ ] **[#257](https://github.com/skaft-software/octet/issues/257) pi-compat: select and run one exact ordered aggregate runtime** — Pi & runtime · Acceptance audit · enhancement
- [ ] **[#261](https://github.com/skaft-software/octet/issues/261) extensions: publish v0.8 runtime resource and latency release evidence** — Pi & runtime · Acceptance audit · enhancement
- [ ] **[#262](https://github.com/skaft-software/octet/issues/262) migration: add typed adapter discovery and MigratedSetup result transport** — Pi & runtime · Acceptance audit · enhancement
- [ ] **[#263](https://github.com/skaft-software/octet/issues/263) extensions: add typed provider-retry disposition hook** — Pi & runtime · Acceptance audit · enhancement
- [ ] **[#264](https://github.com/skaft-software/octet/issues/264) extensions: add bounded tool-progress presentation enrichment** — Pi & runtime · Acceptance audit · enhancement
- [ ] **[#265](https://github.com/skaft-software/octet/issues/265) extensions: add namespaced pre-persistence turn metadata enrichment** — Pi & runtime · Acceptance audit · enhancement
- [ ] **[#268](https://github.com/skaft-software/octet/issues/268) extensions: complete bounded long-running command progress conformance** — Pi & runtime · Acceptance audit · enhancement
- [ ] **[#269](https://github.com/skaft-software/octet/issues/269) extensions: add provider registration and dynamic catalog lifecycle** — Pi & runtime · Acceptance audit · enhancement
- [ ] **[#270](https://github.com/skaft-software/octet/issues/270) extensions: add bounded provider stream proxy with backpressure and cancellation** — Pi & runtime · Acceptance audit · enhancement

### 3c. Migration (3)

- [ ] **[#156](https://github.com/skaft-software/octet/issues/156) Extension: pi import adapter (octet-import-pi)** — Pi & runtime · Acceptance audit
- [ ] **[#157](https://github.com/skaft-software/octet/issues/157) Host ingestion: apply_migrated_setup with idempotent merge + backups** — Pi & runtime · Acceptance audit
- [ ] **[#279](https://github.com/skaft-software/octet/issues/279) cli: wire octet migrate import through typed adapter and host ingestion** — Pi & runtime · Acceptance audit · enhancement

### 3d. TUI / first run (4)

- [ ] **[#274](https://github.com/skaft-software/octet/issues/274) tui: add guided first-run local-model setup flow** — Daily driver · Acceptance audit · enhancement
- [ ] **[#275](https://github.com/skaft-software/octet/issues/275) cli: add deterministic provider setup command and non-interactive recovery diagnostics** — Daily driver · Acceptance audit · enhancement
- [ ] **[#282](https://github.com/skaft-software/octet/issues/282) tui: migrate model, resume, extension, and completion pickers to shared chrome** — Daily driver · Acceptance audit · enhancement
- [ ] **[#283](https://github.com/skaft-software/octet/issues/283) tui: migrate help, status, context, cost, and cache reports to shared chrome** — Daily driver · Acceptance audit · enhancement

### 3e. Infra (2)

- [ ] **[#112](https://github.com/skaft-software/octet/issues/112) build: retain CI/profile measurements and usable-symbol evidence** — Qualification & repair · Acceptance audit · enhancement
- [ ] **[#173](https://github.com/skaft-software/octet/issues/173) First-class lifecycle feedback for cold-starting llama.cpp and vLLM endpoints** — Daily driver · Acceptance audit · enhancement

---

## Phase 4 — later (Queue: Later, 58)

### 4a. Maintainability refactors — behavior-preserving (20)

- [ ] **[#111](https://github.com/skaft-software/octet/issues/111) ci: audit production unwrap and expect usage incrementally** — Qualification & repair · Accepted / Next · effort L · Independent review · enhancement
- [ ] **[#162](https://github.com/skaft-software/octet/issues/162) Benchmark evidence: qualify clean-checkout reproduction and report fixtures** — Evidence & launch · Accepted / Next · documentation
- [ ] **[#311](https://github.com/skaft-software/octet/issues/311) refactor sexy-tui-rs: split rich-text block, inline, code, table, and diff rendering** — Qualification & repair · Accepted / Next · enhancement
- [ ] **[#314](https://github.com/skaft-software/octet/issues/314) refactor octet-coding-agent: split provider presets, catalogs, pricing, compatibility, and credential storage** — Qualification & repair · Accepted / Next · enhancement
- [ ] **[#315](https://github.com/skaft-software/octet/issues/315) refactor octet-coding-agent: split documentation, system prompts, skills, prompt formats, expansion, and provenance** — Qualification & repair · Accepted / Next · enhancement
- [ ] **[#316](https://github.com/skaft-software/octet/issues/316) refactor octet-coding-agent: expose readable idle and active interactive state machines** — Qualification & repair · Accepted / Next · enhancement
- [ ] **[#317](https://github.com/skaft-software/octet/issues/317) refactor octet-coding-agent: split RPC framing, typed protocol, commands, events, projections, and bash execution** — Qualification & repair · Accepted / Next · enhancement
- [ ] **[#318](https://github.com/skaft-software/octet/issues/318) refactor octet-coding-agent: split host protocol, transport, routing, run orchestration, media, and session authority** — Qualification & repair · Accepted / Next · enhancement
- [ ] **[#319](https://github.com/skaft-software/octet/issues/319) refactor octet-coding-agent: split extension discovery, trust, runtime, lifecycle, hooks, commands, and presentation** — Qualification & repair · Accepted / Next · enhancement
- [ ] **[#320](https://github.com/skaft-software/octet/issues/320) refactor octet-coding-agent: split extension package transactions and Pi installation ownership** — Qualification & repair · Accepted / Next · enhancement
- [ ] **[#321](https://github.com/skaft-software/octet/issues/321) refactor octet-coding-agent serve integration: split startup, routing, projects, sessions, conversations, and runs** — Qualification & repair · Accepted / Next · enhancement
- [ ] **[#322](https://github.com/skaft-software/octet/issues/322) refactor octet-coding-agent serve integration: split run projection, tool activity, durable recovery, and resources** — Qualification & repair · Accepted / Next · enhancement
- [ ] **[#323](https://github.com/skaft-software/octet/issues/323) refactor octet-coding-agent serve integration: split repository and pull-request services** — Qualification & repair · Accepted / Next · enhancement
- [ ] **[#324](https://github.com/skaft-software/octet/issues/324) refactor octet-coding-agent: split session paths, catalogs, metadata, transcript scans, commands, export, and maintenance** — Qualification & repair · Accepted / Next · enhancement
- [ ] **[#325](https://github.com/skaft-software/octet/issues/325) refactor octet-coding-agent: split migration discovery, scanners, planning, reporting, and hydration** — Qualification & repair · Accepted / Next · enhancement
- [ ] **[#326](https://github.com/skaft-software/octet/issues/326) refactor octet-coding-agent: split model/tool display, temporal run state, formatting, and changed-file projection** — Qualification & repair · Accepted / Next · enhancement
- [ ] **[#328](https://github.com/skaft-software/octet/issues/328) refactor octet-coding-agent: split update checking, actions, install detection, package layout, and process execution** — Qualification & repair · Accepted / Next · enhancement
- [ ] **[#330](https://github.com/skaft-software/octet/issues/330) refactor octet-coding-agent TUI: split composer, attachment ledger, paste handling, and picker flows** — Qualification & repair · Accepted / Next · enhancement
- [ ] **[#331](https://github.com/skaft-software/octet/issues/331) refactor octet-coding-agent TUI: split terminal capabilities, lifecycle, backend, events, and signal restoration** — Qualification & repair · Accepted / Next · enhancement
- [ ] **[#332](https://github.com/skaft-software/octet/issues/332) refactor octet-coding-agent TUI: split theme loading, color policy, panel filtering, layout, and rendering** — Qualification & repair · Accepted / Next · enhancement

### 4b. Serve & companions (12)

- [ ] **[#58](https://github.com/skaft-software/octet/issues/58) octet-serve: keyboard shortcut hints and global shortcuts** — Serve & companions · Accepted / Next
- [ ] **[#65](https://github.com/skaft-software/octet/issues/65) octet-serve: multi-pane workspace layout** — Serve & companions · Accepted / Next
- [ ] **[#70](https://github.com/skaft-software/octet/issues/70) octet-serve: reasoning effort controls and per-model preferences** — Serve & companions · Accepted / Next
- [ ] **[#71](https://github.com/skaft-software/octet/issues/71) octet-serve: GitHub PR notifications and review panel** — Serve & companions · Accepted / Next
- [ ] **[#127](https://github.com/skaft-software/octet/issues/127) Design review: make the Octet Serve command center feel intentional** — Serve & companions · Accepted / Next · enhancement
- [ ] **[#180](https://github.com/skaft-software/octet/issues/180) octet serve: allow naming a session at startup** — Serve & companions · Accepted / Next · enhancement
- [ ] **[#221](https://github.com/skaft-software/octet/issues/221) companion: recover and deliver the full native iOS Serve client** — Serve & companions · Accepted / Next
- [ ] **[#341](https://github.com/skaft-software/octet/issues/341) Serve: qualify cross-boundary security and reliability invariants** — Qualification & repair · Accepted / Next · enhancement
- [ ] IN PROGRESS — **[#382](https://github.com/skaft-software/octet/issues/382) TUI: qualify ANSI256 and SSH colour behavior on current releases** — Daily driver · Acceptance audit · Independent review
- [ ] **[#394](https://github.com/skaft-software/octet/issues/394) companion: deliver the native macOS Serve client end to end** — Serve & companions · Accepted / Next · Security review · enhancement
- [ ] **[#395](https://github.com/skaft-software/octet/issues/395) Security: independently audit the full Serve and companion candidate** — Serve & companions · Accepted / Next · Security review · enhancement
- [ ] **[#396](https://github.com/skaft-software/octet/issues/396) Serve: explore lifecycle, isolation and reconnect qualification** — Serve & companions · Exploring · Security review · enhancement

### 4c. Themes — exploring, unscheduled (7)

- [ ] **[#414](https://github.com/skaft-software/octet/issues/414) Epic: user-authored themes, variants, live reload, and extension theming** — Daily driver · Exploring · enhancement
- [ ] **[#415](https://github.com/skaft-software/octet/issues/415) themes: enable bounded theme-file discovery, resolution, and selection** — Daily driver · enhancement
- [ ] **[#416](https://github.com/skaft-software/octet/issues/416) themes: ship the default theme as a documented variant reference** — Daily driver · documentation
- [ ] **[#417](https://github.com/skaft-software/octet/issues/417) themes: separate terminal appearance from named theme selection** — Daily driver · enhancement
- [ ] **[#418](https://github.com/skaft-software/octet/issues/418) themes: live-reload the active theme on file change** — Daily driver · enhancement
- [ ] **[#419](https://github.com/skaft-software/octet/issues/419) extensions: contribute theme resources and publish the semantic role vocabulary** — Daily driver · enhancement
- [ ] **[#420](https://github.com/skaft-software/octet/issues/420) extensions: host-mediated theme selection capability** — Daily driver · enhancement

### 4d. Benchmarks & evidence (5)

- [ ] **[#191](https://github.com/skaft-software/octet/issues/191) Benchmark: publish reproducible peak RSS and process-footprint results** — Evidence & launch · Accepted / Next
- [ ] **[#192](https://github.com/skaft-software/octet/issues/192) Benchmark: run a valid Terminal-Bench 2.1 and Harbor Index campaign** — Evidence & launch · Exploring
- [ ] **[#194](https://github.com/skaft-software/octet/issues/194) Benchmark: prepare and run the Terminal-Bench 4 campaign** — Evidence & launch · Exploring
- [ ] **[#195](https://github.com/skaft-software/octet/issues/195) Usability: record voluntary install and daily-driver failure reports** — Evidence & launch · Exploring
- [ ] **[#219](https://github.com/skaft-software/octet/issues/219) Benchmark: measure baseline and per-integration context footprint** — Evidence & launch · Accepted / Next

### 4e. Research spikes & explorations (10)

- [ ] **[#4](https://github.com/skaft-software/octet/issues/4) Spike: Import sessions from Pi Coding Agent** — Later · Exploring · enhancement
- [ ] **[#23](https://github.com/skaft-software/octet/issues/23) Design and evaluate an optional LSP-backed code-intelligence layer** — Later · Exploring · enhancement
- [ ] **[#42](https://github.com/skaft-software/octet/issues/42) Inter-extension event bus** — Later · Exploring · enhancement
- [ ] **[#119](https://github.com/skaft-software/octet/issues/119) Spike: design and evaluate durable agent graphs for reliable local-model workflows** — Later · Exploring · enhancement
- [ ] **[#150](https://github.com/skaft-software/octet/issues/150) tui: make tok/s timing request-scoped and independently reproducible** — Daily driver · Accepted / Next · enhancement
- [ ] **[#174](https://github.com/skaft-software/octet/issues/174) Upgrade Octet to Rust 2024 edition** — Later · Exploring · enhancement
- [ ] **[#175](https://github.com/skaft-software/octet/issues/175) feat: add Codex-only `/fast` slash command** — Later · Exploring · enhancement
- [ ] **[#184](https://github.com/skaft-software/octet/issues/184) Spike: evaluate Codex-style verified background sessions without an agent pane** — Serve & companions · Exploring · enhancement
- [ ] **[#347](https://github.com/skaft-software/octet/issues/347) Research: Claude capability audit — separately scheduled follow-up** — Later · Exploring · effort Epic · Human decision · type/epic
- [ ] **[#348](https://github.com/skaft-software/octet/issues/348) Research: reconcile verified Anthropic discovery capabilities and model limits** — Later · Exploring · effort M · Independent review · bug

### 4f. Migration adapters — low (2)

- [ ] **[#160](https://github.com/skaft-software/octet/issues/160) Extension: Cline import adapter** — Later · Exploring
- [ ] **[#161](https://github.com/skaft-software/octet/issues/161) Extension: aider import adapter** — Later · Exploring

### 4g. Runtime / media (2)

- [ ] **[#343](https://github.com/skaft-software/octet/issues/343) sessions/media: store durable assets and bound provider request projection** — Later · Accepted / Next · enhancement
- [ ] **[#344](https://github.com/skaft-software/octet/issues/344) agent/runtime: parallelize multimodal reads and right-size Tokio workers** — Later · Accepted / Next · bug

---

## Epics — open umbrellas, track them (17)
Members of each epic sit in the phases above by their own queue (e.g. #190 → 2b/2e, #2 → 2a, #345 → 2c, #153 → 2d + Phase 1 #276, #158/#163 → Phase 1 + 3b/3c).

- [ ] **[#2](https://github.com/skaft-software/octet/issues/2) Epic: complete deferred Pi provider and protocol support** — Pi & runtime · Accepted / Next · enhancement
- [ ] **[#4](https://github.com/skaft-software/octet/issues/4) Spike: Import sessions from Pi Coding Agent** — Later · Exploring · enhancement
- [ ] **[#19](https://github.com/skaft-software/octet/issues/19) Maintainability: small ownership improvements and targeted qualification** — Qualification & repair · Accepted / Next · enhancement
- [ ] **[#46](https://github.com/skaft-software/octet/issues/46) Epic: typed extension mutation hooks** — Pi & runtime · Accepted / Next · enhancement
- [ ] **[#48](https://github.com/skaft-software/octet/issues/48) Epic: extension-provided provider runtime** — Pi & runtime · Accepted / Next · enhancement
- [ ] **[#125](https://github.com/skaft-software/octet/issues/125) Epic: guided first-run provider setup** — Daily driver · Accepted / Next · enhancement
- [ ] **[#153](https://github.com/skaft-software/octet/issues/153) Epic: inline terminal images in the TUI** — Daily driver · Accepted / Next · enhancement
- [ ] **[#158](https://github.com/skaft-software/octet/issues/158) Epic: migration CLI import surface** — Pi & runtime · Accepted / Next · type/epic
- [ ] **[#163](https://github.com/skaft-software/octet/issues/163) Epic: hook coverage — session lifecycle, post-mutation, progress events** — Pi & runtime · Accepted / Next · enhancement
- [ ] **[#189](https://github.com/skaft-software/octet/issues/189) Epic: professional release pipeline and npm/Homebrew distribution** — Evidence & launch · Exploring · effort Epic · Security review · type/epic
- [ ] **[#190](https://github.com/skaft-software/octet/issues/190) Epic: v0.8 install and use unchanged Pi extensions end to end** — Pi & runtime · Accepted / Next · Security review · type/epic
- [ ] **[#196](https://github.com/skaft-software/octet/issues/196) Epic: Serve hardening; native-client product separately scheduled** — Serve & companions · Accepted / Next · type/epic
- [ ] **[#198](https://github.com/skaft-software/octet/issues/198) Roadmap: dependable core, media input and language-neutral extensions** — Daily driver · Accepted / Next · documentation
- [ ] **[#236](https://github.com/skaft-software/octet/issues/236) Epic: TUI information architecture and command chrome** — Daily driver · Accepted / Next · enhancement
- [ ] **[#345](https://github.com/skaft-software/octet/issues/345) Epic: full Codex-comparable computer use after Pi compatibility** — Computer use · Accepted / Next · effort Epic · Security review · enhancement
- [ ] **[#351](https://github.com/skaft-software/octet/issues/351) Tracking: native scrollback, streamed Markdown and remaining TUI acceptance** — Daily driver · Accepted / Next · effort Epic · Human decision · type/epic
- [ ] **[#352](https://github.com/skaft-software/octet/issues/352) Planning: reconcile models/providers UX with integrated local setup** — Daily driver · Accepted / Next · effort Epic · Human decision · type/epic

---

## External — needs org-level setup, no code (3)

- [ ] **[#189](https://github.com/skaft-software/octet/issues/189) Epic: professional release pipeline and npm/Homebrew distribution** — Evidence & launch · Exploring · effort Epic · Security review · type/epic
- [ ] **[#213](https://github.com/skaft-software/octet/issues/213) Distribution: publish Octet through npm trusted publishing** — Evidence & launch · Exploring · effort L · Security review
- [ ] **[#214](https://github.com/skaft-software/octet/issues/214) Distribution: publish and verify a Homebrew tap/formula** — Evidence & launch · Exploring · effort L · Security review

---

## Orphan — open issue not on the board (1)
- [ ] **[#430](https://github.com/skaft-software/octet/issues/430) Docs still document removed skill tools (`search_skills`, `load_skill`, `read_skill_resource`)** — doc fix: `docs/design/octet-coding-agent.md` + `docs/instructions.md` list tools deleted in `4714e93`; `docs/tools.md` already matches the code. Minutes of work.

---

## Not promised (roadmap §Not promised / §Later) — out of scope unless selected
- Unchanged Pi extensions **without** compat layers, computer-use parity, benchmark superiority in the next release.
- Needs separate selection: Pi compatibility, themes, Serve, companions, voice, computer use.
- Epic #414 is explicit: user-authored themes are unscheduled possibilities, not a v0.8 commitment.

## Done — do not re-implement (16 board-Done items + 168 closed issues)
Board-Done: extension session lifecycle API; tool-policy provenance; MigratedSetup types crate;
sexy-tui-rs composer framework; Azure OpenAI Responses routing; Cloudflare AI Gateway routing;
provider schema-invalid argument recovery; provider runtime split; TUI shared chrome contract;
bootstrap/TUI/shutdown suites; transactional provider-setup service; SessionStart/SessionEnd hooks;
ownership-baseline audit; atomic paste/attachment Backspace delete; state-notice + picker fix.
All 168 closed issues are complete — treat closed as done.

---

## Fastest vibe-code path
1. **Phase 0** — evidence/qualification + real code bugs: #429 (Enter invokes selected slash command — keymap fix), #428 (web-search shared deadline / cooperative I/O — known unshipped fix direction), #379 (media attachments), #377 (browse focus/flicker).
2. **#430** doc fix — minutes.
3. **Phase 1** — #179 already in progress; then TUI cluster #349 → #392 → #393 → #381, #276 image foundation (unblocks 2d), #424 provider self-description, #313 config diagnostics, #383/#378 browser/desktop policy groundwork.
4. **Phase 3** audits run as their code lands — each is a verification checklist, not code to write.
5. **Phase 2 in epic order**: 2a providers → 2b pi-compat → 2e extension runtime → 2d TUI images → 2c computer use.
6. **Phase 4** — refactors are mechanical and parallel-safe; then serve polish, benchmarks, spikes. External items need org access.

**Count check:** 9 + 11 + 25 + 20 + 58 + 1 orphan = 124 leaf items; the 17 epics
overlap those leaves as umbrellas; total open issues = 142.
