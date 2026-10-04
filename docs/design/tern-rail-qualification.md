# Tern RAIL — unfinished implementation and verification

**Status:** Authorized RAIL implementation; independent presentation deliverable,
not complete native parity and not release-ready qualification.

The acceptance scope remains **all 37 existing octet primary-screen/native-
scrollback surface families**, including permission popups and runtime extension
contracts. This ledger does not reduce that scope to the changed components.
Pre-approval audit wording is historical: RAIL was subsequently selected and
implementation authorized. No new OMP-only backend workflows, provider campaign
or performance campaign are implied.

The [Tern contract](../tern.md) describes the current adapter. The shared
[presentation](octet-presentation.md), [TUI](octet-tui.md) and
[ordinary-surface](octet-command-picker-surfaces.md) contracts retain host owners,
accounting, durable source and authority boundaries. Native ANSI content is a
deliberate compatible projection, not an ANSI TUI running below Tern; it does
not establish a missing semantic input route.

## Reading this ledger

**Changed** means source-level RAIL adapter work, **preserved** means an existing
owner/contract remains, **blocked** names an absent adapter or protocol route,
and **unverified** means actual qualification remains. These are not pass/fail
results. A changed or preserved row can also be blocked in one dimension and
unverified in all its applicable native journeys.

Source paths below are relative to `crates/octet-coding-agent/src/` unless a
crate is named. Bare adapter/render/navigation filenames resolve under
`tui/view/`; `commands.rs`, `cli.rs` and `extension_package.rs` are at the
source root. `tui/view/tern.rs` owns the retained projection;
`tui/view/tern_input.rs` routes shared input; `tui/view/renderer_model.rs`
publishes accepted source. File/function references are preferred to audit-era
line numbers because implementation is ongoing.

## Required qualification for every applicable family

Each row still requires source/owner mapping, actual input routing, tree and
protocol-byte checks, relevant real-binary PTY checks, and native pixel and
interaction checks on the selected candidate/version. Fixtures and protocol
schema alone do not complete this chain.

Apply every relevant dimension, explicitly recording inapplicable cases rather
than inventing states:

- Ready; loading/waiting; empty/no matches; unavailable/disabled with reason;
  success; recoverable error; cancellation admission and separately acknowledged
  cancellation; EOF/shutdown; reopen/resume.
- Narrow, short and wide panes; resize/reflow; zoom; light/dark; reduced motion;
  streaming updates and finalization without lost source or stale reader state.
- Keyboard and pointer; remapped and disabled bindings; focus away/back;
  hidden-pane return; stale action/edit; source/catalogue replacement; owner and
  request transitions; credit starvation, eviction and explicit fallback.
- Exact draft/caret/selection/attachment fidelity; truthful missing facts;
  disclosure before output transport; credential privacy; acknowledged visible
  consent; cancellation versus actual runtime/descendant settlement.
- Accessibility/IME and terminal-owned intrinsic actions where applicable.
  Tern 0.3.1 is **untested**. Prior 0.4.0 specimens do not qualify 0.3.1 or the
  final product adapter.

## All 37 surface families

### Startup, discovery and sessions

- **S01 — Silent startup/discovery, fresh/resumed/forked history, ready,
  setup/error. Changed/preserved; unverified.** `tern.rs::project`,
  `renderer_model.rs`, `tern_input.rs`: conversation-local identity and retained
  history. Pending native startup still suppresses ordinary editor gestures;
  startup editability/atomic history install needs actual native qualification.
- **S02 — Welcome, version/changelog/update, permissions, model-less commands.
  Changed; unverified.** `tern_welcome.rs`, `tui/splash.rs::native_png`: compact
  40×20 content-addressed canonical raster, wrapping identity and warning-class
  full access. Geometry, zoom and reduced-motion journeys remain unqualified.
- **S03 — Setup provider/method/subscription, endpoint/environment/manual-model
  fields, review/save/edit/back. Changed/preserved; unverified.**
  `modes/interactive/onboarding.rs`, `tui/pickers.rs`, `tern_picker_sheet.rs`,
  `tern_prompt.rs`: ordinary choices/fields use existing owners; keys remain
  host-private. No actual provider save or onboarding execution is asserted.
- **S04 — Login/logout, browser/device auth, credential precedence/replacement,
  retry/error/cancel. Changed/preserved; partially evidenced, unverified.**
  `modes/interactive.rs::await_codex_login`/`login_codex_catalog`,
  `auth/codex/login.rs`: task-local single-slot progress owns a transient native
  document without suspending rendering or storing instructions in history.
  Public browser/device instructions, fallback and save/commit phases have
  synthetic owner tests; pre-commit cancellation drops OAuth and a late cancel
  cannot misreport a completed credential save. Real OAuth, credential saves,
  native link interactions and OS browser effects remain unqualified.
- **S05 — Model catalogue/provider scopes/current facts, cycling and deferred
  switch. Preserved/fenced; unverified.** `tern_picker.rs`, `tern_input.rs`:
  source-backed public-facts preview and first-selectable focus; catalogue
  gestures are epoch/content fenced. No account/role/fallback manager invented.
- **S06 — Exact thinking levels, reasoning mode, live/deferred selection, fast
  tier. Preserved; unverified.** `tern_picker.rs::thinking_node`,
  `modes/interactive.rs`: compact supported choices and resolved bindings.
  Local queued selection is not provider acknowledgement.
- **S07 — Theme discovery/preview/rollback/save, Auto/Light/Dark, defaults and
  ordered scoped models. Preserved; blocked/unverified dimensions.**
  `tern_theme.rs`, `tern_picker.rs`, `commands.rs`: host persistence and factual
  reports remain. General editable prefs/scoped-model editor are absent;
  native per-turn RGB tint is protocol-limited.
- **S08 — Help/hotkeys/status/context/cost/cache/warming/session/changelog/goals/
  settings/scopes/resource reports. Preserved; unverified.** `tern.rs::report`,
  `tui/view.rs`: semantic Markdown/context and native ANSI
  documents. Application report/document offsets are not native geometry;
  optional scrolling needs independent checks, including legacy any-key owners.
- **S09 — Session workspaces, fuzzy/phrase/regex filtering, explicit transcript
  search, all existing sorts/named/path filters and resume restriction.
  Preserved/fenced; unverified.** `tern_sessions.rs`, `tern_picker.rs`,
  `tern_input.rs`: real metadata preview and current host catalogue. Metadata
  is not a conversation preview; refresh and cross-workspace routes need checks.
- **S10 — Rename/name, recoverable trash/delete consent, current protection,
  fork/whole conversation, clone/export. Preserved; blocked/unverified.**
  `tern_session_edit.rs`, `tern_picker.rs`, `tui/view.rs`, `modes/interactive.rs`:
  native rename has a bounded, source/catalogue/revision-fenced editor and
  resolved controls; persistence remains with the existing host picker driver.
  Its field/actions precede long metadata in narrow panes. Trash consent remains
  native ANSI content with no new positive pointer authority. PTY covers resumed
  and forked active-session export; the earlier lock-deadlock hypothesis was not
  reproduced after correcting the session fixture/transport. Actual Tern also
  exported executed local-command history from a fresh active session; the
  complete mutation/error matrix remains unqualified. No restore browser or
  permanent deletion is claimed.

### Composer and conversation

- **S11 — Multiline editor/caret/selection, word/line/seek, kill/yank, undo/redo,
  recall and external editor. Preserved; blocked/unverified dimensions.**
  `tern.rs::composer`, `tern_input.rs`, `tui/keymap.rs`: program hello advertises
  `edit`, as required by the current official Tern docs and installed runtime.
  Actual pointer caret/selection replacement and host undo pass. Native edits
  now preserve host undo history; UTF-16/grapheme boundaries and stale lengths
  are checked. Same-length stale edits still lack source revision in the wire;
  request epochs do not solve it. IME/wrapping remain terminal-owned.
- **S12 — Slash/dynamic commands, skills/templates, @ files and all existing
  path forms/chaining/escaping. Preserved/fenced; unverified.**
  `tern_completion.rs`, `tern_input.rs`: source IDs, highlight versus activation
  and draft fencing remain. No OMP-only URL/emoji/model-mention providers added.
- **S13 — Bracketed/clipboard/large paste, dropped paths, image/audio admission,
  PDF reference and chip delete/undo/recall. Preserved; blocked/unverified.**
  `tui/composer/attachments.rs`, `tui/composer/composition.rs`, `tern_input.rs`: host paste
  ledger remains; native Edit has no paste intent and cannot stand in for media
  admission. Native mutations touching admitted chip masks are rejected/resynced
  until ledger-aware chip deletion/undo is implemented. This is a local
  implementation gap, not a claim that Tern cannot report the selection.
  Typing a path never implies upload consent.
- **S14 — FIFO follow-ups, steering/claim/recall, queued commands, /answer,
  goals and deferred model/session/settings/reload. Preserved; unverified.**
  `tern.rs::project`, `modes/interactive.rs`, `tui/view.rs`: compact native ANSI
  pending hint; original settlement/session-parking rules, not new queue controls.
- **S15 — Assistant/user Markdown, code/tables/math/links/diagrams,
  live/final/replayed prose. Changed; unverified.** `tern.rs::assistant_node`
  and `block_node`: unboxed Col/direct stable Md, `omp.user` right bubble and
  96ch measure. Tightening preserves fence contents; diagram/fence dialect,
  streaming/replay and native copy need actual checks. Per-turn tint remains limited.
- **S16 — Real reasoning, live/settled/partial/interrupted/failed traces,
  headings/code/math/lists/quotes and global/per-trace expansion.
  Changed; unverified.** `tern.rs::block_node`,
  `assistant_block.rs::reasoning_markdown_projection`: semantic headings,
  stable Markdown and host expansion; no empty trace or invented timing.
  Trace settlement is distinct from run success.
- **S17 — Working, provider queued/loading/ready, network wait, retry/backoff/
  countdown, compaction and post-answer activity. Changed; unverified.**
  `tern.rs::working_row`, `reasoning_render.rs`: actual lifecycle/retry labels
  and observed countdown, no synthetic deadline or premature completion.
  Verify transitions and timer wakes through actual native ownership.

### Tools, disclosure, outcomes and navigation

- **S18 — Every actual/extension tool, read/search targets, edit/write diffs,
  tool progress and groups. Changed/preserved; unverified.**
  `tern.rs::tool_node`/`block_node`, `tool_render.rs`: deterministic display
  labels, progress decoration and grouped-child visibility. This is not a claim
  of complete ANSI/native grouping geometry or future extension renderer parity.
- **S19 — Bash/exec/local !, excluded !!, full multiline commands and all
  execution outcomes. Changed; unverified.** `tern.rs::command_node`/
  `tool_node`/`shell_node`, `tern_images.rs`: full wrapping Code immediately;
  global Ctrl+O gates captured text, nodes, image preparation/hash and upload.
  Second toggle removes projection, not source. Actual protocol bytes are required.
- **S20 — Tool images and disabled/rejected/unsupported placeholders.
  Changed/preserved; unverified.** `tern_images.rs`: validated bounded,
  content-addressed opt-in media, dedup and pre-transport command disclosure.
  Submitted-media thumbnail/playback equivalence is not established.
- **S21 — Compaction live/summary/global expansion and context/accounting.
  Changed/preserved; unverified.** `tern.rs::block_node`/`report`,
  `tui/context.rs`: summary follows host expansion/global policy; context uses
  real quantities. Disclosure/copy/accounting transitions need actual checks.
- **S22 — Completed/warnings, failure/reason, interruption, needs-input,
  recovered previous-attempt output. Preserved; unverified.**
  `tern.rs::outcome_parts`, `modes/interactive.rs`: semantic outcomes and bounded
  redacted failure. Visible answer/animation is not settlement; recovered partial
  text is not current answer or new accounting.
- **S23 — Usage/cost/context/session totals and token rate. Preserved;
  unverified.** `tern.rs`, `tern_inference_tests.rs`: available `N.N tok/s`,
  server timing then accepted client decode estimate; missing pricing/rate absent,
  provenance only /status and no E2E fallback. Fixtures are not live measurements.
- **S24 — Pages/lines/top/tail, prompt jumps, pinned reader/new-output,
  search next/previous/close, scrollbar/drag. Changed protocol boundary;
  blocked/unverified dimensions.** `transcript_navigation.rs`, `tern.rs`,
  `crates/octet-tern/src/wire.rs::ScrollBy` and `client.rs::supports_feature`:
  optional scroll forwarding requires actual `hello.features.scroll`.
  No-advertisement behavior, modal targeting and streaming reader stability need
  checks; this does not implement semantic search, prompt-jump or anchor parity.
- **S25 — Native/app-owned selection/copy, message copy, links/file open and
  clipboard. Preserved; blocked/unverified.** `transcript_selection.rs`,
  `tui/keymap.rs`, `tern.rs`: semantic source remains authoritative. Native
  pixels cannot be guessed from ANSI cell pointer coordinates; native selection,
  copy and semantic pointer geometry require protocol/terminal evidence.

### Extensions, workers and authority

- **S26 — Installed extensions, activation/authority/version/source,
  unavailable/shadowed/external management. Preserved; unverified.**
  `modes/interactive/extension_menu.rs`, `tern_picker_sheet.rs`: source-backed
  compact choices. Activation is not trust; root-only authority and revalidation remain.
- **S27 — Extension root/nested/generated/empty/refreshed menus,
  recommendations, arguments/progress/reports/errors. Changed/fenced;
  unverified.** `modes/interactive/extension_menu.rs`, `tern_picker.rs::id`,
  `tern_picker_sheet.rs`, `tern_prompt.rs`: catalogue-bound ordinals, full
  selected facts and existing back stack; bounded source menus, no fake dashboard.
- **S28 — API 0.4 fullscreen/header/footer/above/below/editor, rescue/close,
  geometry/reload. Changed/preserved; unverified.** `remote_ui.rs`,
  `tern.rs::project`/`editor_focused`, `tern_input.rs`: validated native ANSI
  projection; remote editor/fullscreen suppress ordinary composer/actions and
  preserve host owner/generation/geometry fences and Ctrl+G rescue.
  Arbitrary extension TSP is not a supported contract.
- **S29 — Subagent queued/running/waiting/stopping/settled/failure/limits/
  timeout/stopped/detached/approval/restart activity. Changed; unverified.**
  `tern_agents.rs::transcript`, `renderer_model.rs`: one conversation-local
  block, state counts/four host token lines/overflow, provisional `~`, no pinned
  duplicate. Hydrated evidence is neutral; no fabricated percentage or cost.
- **S30 — Worker roster/filter/groups, detail/read-only transcript,
  missing/foreign/stale reference, inspect/stop/wait/reattach/preview panes.
  Preserved/fenced; blocked/unverified dimensions.** `tern_picker.rs`,
  `tui/pickers.rs`, `modes/interactive.rs`: source-bound refresh fencing and
  authoritative reference checks. Pane opening remains a blocked preview;
  stop acknowledgement is not settlement; idle/active Ctrl+X needs qualification.
- **S31 — Edit/write/shell/local effect approval, exact policy/intent/grant,
  deny/expire/timeout. Preserved; unverified.** `tern_picker.rs::interactive`,
  `renderer_geometry.rs`, `tern.rs`: consent stays outside compact sheets and
  native positive pointer routing. One bounded ANSI body retains the existing
  host source-action receipt and exact acknowledgement gate; frame acknowledgement
  is not native geometric visibility. Baseline native visibility remains unqualified.
- **S32 — Tool/extension yes-no/destructive questions, first scoped
  preapproval and stop-all consent. Preserved; unverified.**
  `extensions/confirmation.rs`, `extensions/commands.rs`, `tui/pickers.rs`:
  same sheet family, distinct actual owner/default; Escape/EOF/missing frontend
  deny, later prompts remain asked. Generic consent is not a typed effect grant.
- **S33 — Extension authority grant/revoke, implicit/explicit/source-bound/
  one-shot/blocked. Preserved; unverified.** `modes/interactive/extension_menu.rs`,
  `cli.rs`, `extension_package.rs`: real OS-authority consequence and separate
  activation. Broker does not confine granted code; no invented startup-trust popup.
- **S34 — Ordinary/secret/active FIFO input, type/paste/backspace/submit/
  cancel/error/overflow. Changed; unverified.** `tern_prompt.rs`,
  `tui/view.rs::begin_tool_input`/`edit_tool_input`, `tui/pickers.rs`,
  `modes/interactive.rs`: shared epoch-fenced 4096-byte ordinary editor; parent
  draft untouched. Secrets HOST-PRIVATE, no editor/value/mask/native confirm;
  secret characters and raw Enter/Esc bypass remapped/disabled picker bindings.
  Underlying panel gestures are suppressed while a temporary owner is active,
  so their controls cannot synthesize submission into a secret request.
  PTY covers bounded ordinary/secret input, overflow recovery, sequential
  request fencing and close; actual native prompt/secret journeys and
  same-length edit revision remain unqualified.
- **S35 — Provider save/replacement, session trash and non-typed decisions.
  Preserved; unverified.** `modes/interactive/onboarding.rs`, `tui/view.rs`: explicitly scoped
  existing confirmations keep owner/default/consequence. They must not inherit
  typed tool-grant authority merely by using the same visual family.

### Lifecycle and terminal ownership

- **S36 — Close/interrupt/draft clear/dispatch, EOF/errors/signals,
  suspend/external editor, resize/focus/hide/eviction/credits/fallback.
  Changed/preserved; partially tested.** `tern.rs`, `tern_input.rs`, `tui/keymap.rs`,
  `tui/terminal/lifecycle.rs`: one input/render owner, retained history,
  bounded coalescing and explicit fallback. Hidden presentation is suspended
  even with remaining credit. A focus counter arriving during materialization
  cannot be consumed without reasserting focus (deterministic regression).
  Resize reasserts the current owner even without a visibility transition.
  Native startup enables/restores focus reporting; supported pointer-focus
  requests cannot change the host-selected owner. The shared frontend uses the
  supported level-triggered tty reader, avoiding a stranded readiness edge
  during resize/protocol bursts. Idle teardown cannot wait for new input.
  Three actual Tern 0.4.0 runs passed nine owned split/return cycles and nine
  pointer-reselection cases with fresh input visible and echoed by the host.
  Earlier failures are retained; OS-app switching, IME, 0.3.1 and the remaining
  recovery/cancellation-settlement matrix are still untested. Retained pixels
  alone remain insufficient proof.
- **S37 — Accessibility/IME, intrinsic diff/image actions, typography/zoom,
  window/file/system permissions. Preserved terminal ownership;
  blocked/unverified dimensions.** `tern.rs`, `crates/octet-tern/src/wire.rs`:
  native semantic kinds are not evidence of actual accessibility or OS actions.
  Tern 0.3.1 untested; no arbitrary CSS/font/password props or conflation of OS
  permission dialogs with octet effect grants.

## Evidence entrypoints, not results

The coordinating session runs relevant checks on the integrated candidate.
Commands below are reproducible entrypoints, **not passing-run claims**:

```sh
cargo test -p octet-tern --locked
cargo test -p octet-coding-agent --lib --locked tui::view::tern
cargo test -p octet-coding-agent --lib --locked active_tool_input_tests
cargo test -p octet-coding-agent --test tern_native_pty --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Source fixtures in `tern_tests.rs`, `tern_images.rs`, `tern_prompt.rs`,
`tern_agents.rs`, `tern_welcome.rs` and `tern_input.rs` cover proposed tree,
transport and input invariants. Synthetic protocol terminals, inert loopback
records and native specimens are not actual auth/provider/secret/grant/worker
executions and do not prove the full universal matrix.

Seven inherited full-suite failures, 13 inherited strict-Clippy diagnostics and
Codemode instability remain separate baseline blockers. Do not reclassify them
as RAIL regressions without comparison, or claim a UI check clears them. New
failures must be investigated separately. No new performance or live-provider
campaign was undertaken by this documentation deliverable.

## Observed integrated-candidate checks

Current follow-up (partial evidence, not family-wide passes):

- TSP client crate: **25 unit tests and 1 doc test passed**.
- TUI library lane: **940 passed, 1 deliberately ignored** fixture exporter.
- Shared TUI library: **252 passed**; focused text-editor lane: **46 passed**,
  including native range-edit undo/redo, caret-only history preservation and
  invalid grapheme-boundary rejection.
- Native auth owner fixtures: **7 passed**; auth progress/CLI policy fixtures:
  **6 passed**. No OAuth, real credential save or browser authorization was run.
- Active tool input: **6 passed**.
- Real octet binary / synthetic TSP terminal: **32 passed, 0 ignored**.
  This covers fresh/model-less/resumed/forked startup, local reports,
  source-backed catalogue filtering/rename persistence, resumed/forked export,
  Unicode/UTF-16 editing, remapped/disabled controls, ordinary and
  host-private input, loopback streaming/reasoning/cancel/follow-up recall,
  command success/failure/disclosure, credits/visibility/eviction, optional
  kinds/fallback and untrusted extension menus. Loopback responses are synthetic
  protocol evidence, not a real provider or worker campaign. Excluded `!!`
  output is durable non-model-visible configuration metadata, not absent storage.
- Compiled binary + actual Tern **0.4.0** developer headless runtime:
  **61 checks passed**, owned serve exited 0. Observed native mount/context
  alignment, Unicode draft, native pointer range replacement with actual `edit`
  ingress and matching host output, resolved host undo, model pointer/filter/cancel,
  local reports, real session rename typing/cancel/pointer-save with durable
  metadata, actual local process output mount/unmount via Ctrl+O, and native
  export with a durable redacted package containing the executed command history.
  Wide/narrow/short and font-zoom captures were retained privately; the narrow
  rename field/actions were visually inspected after moving long facts below.
  Native ANSI pixels do not expose their terminal text in the DOM, so output
  checks assert body mount/unmount plus inspected pixels, not invented DOM text.
- Separate consecutive-character native rename lane: **10 checks passed**, owned
  serve exited 0; individual edits without explicit frame waits persisted every
  character through the real host driver. This is not IME/full keyboard coverage.
- Shared-reader ANSI regressions: **6 late-terminal-reply PTY tests passed**;
  all **20 startup-frame PTY tests passed across two bounded invocations**
  (19-test slice plus the 30-layout repeated-redraw test); **1 non-TTY ownership
  test passed**. A preliminary waiting-drain experiment failed three startup
  interactions; comparison with the prior reader and corrected no-idle-wait drain
  was retained. One corrected-run HTTP-arrival timeout occurred before terminal
  startup; its isolated rerun and subsequent 19-test slice passed. That failure
  remains recorded rather than being silently classified as inherited.
- Actual pane-return repair: **three independent 18-check runs passed**, owned
  serves exited 0. Each asserted owned original/new pane IDs and three split/
  return cycles plus composer pointer reselection; fresh edits appeared in both
  native editor and host output. The stalled pre-fix observer's blocked write and
  unread ingress, plus earlier failures after pointer reselect, remain private.
  Supported TSP pointer-focus admission has unit/synthetic-PTY evidence; these
  actual pointer runs did not emit that event and are not proof of its emission.
  OS-app switching, IME/accessibility, consent geometry, real auth/grants/workers
  and Tern 0.3.1 remain untested. Accessibility snapshots alone are not passes.
- Documentation-led protocol comparison: three isolated actual-Tern/synthetic-
  client probes confirmed hello omission disables native selection edits, `edit`
  enables them, and `undo` separately enables native undo events. Earlier
  ordinary-typing journeys did not prove pointer-selection editing. The previous
  OMP-derived schema omitted this required opt-in. Octet now advertises only
  `edit`; raw host undo remains binding-resolved. Hello/undo-history regressions
  failed before the fix and passed after. An initial actual-journey exact-text
  assertion failed because Tern's tree inserts a separator around the caret;
  inspected pixels and host bytes showed the correct text. The corrected check
  moves the caret to the end before asserting exact native text.
- After edit opt-in, pane-return **18 checks** and rapid rename **10 checks**
  passed again, with both owned serves exiting 0.
- Formatting and whitespace checks passed. Full workspace and strict Clippy
  were not rerun as passing checks; their inherited blockers remain separate.

Historical baseline fixture evidence (not refreshed runtime qualification):

- Ignored compiled-renderer exporter: **1 passed**, producing eight fixture
  states, not executed commands or provider conversations.
- Tern **0.4.0** offscreen native render: **64 captures** across eight compiled
  fixture states, light/dark, 1200×820, 375×820, 700×420 and narrow font zoom
  16→20. Wide/short/zoomed-narrow contact sheets were visually inspected. This
  remains fixture pixel evidence, not runtime authority or 0.3.1 qualification.

No family has its complete universal matrix cleared by these partial results.
Private screenshots, ingress records and logs are deliberately not published.

## Exact remaining gate

Complete the implemented auth and rename test matrices; extend the repaired
0.4.0 pane-return path to the remaining lifecycle/platform/version cases.
Active-session export passes resumed/forked PTY cases and a fresh-session actual
native journey without backend edits; its full error/mutation matrix still needs
qualification. Native chip mutation/undo, paste intent, same-length stale
composer/temporary edits, semantic transcript pointer geometry, navigation/search/
selection/prompt jumps and full scroll/reader-position parity remain unfinished
or untested. Ordinary editor pointer range replacement is no longer a gap.
Execute every applicable state/input/lifecycle/authority dimension above on the
final integrated candidate, including Tern 0.3.1, and record actual results rather
than fixture counts. Real OAuth, grants, secrets and paid-provider campaigns were
not authorized or performed. Preserve the full S01–S37 scope and separately
resolve inherited release blockers. Until then, RAIL is an independent
implementation deliverable, **not all-surface parity, not terminal-version
qualification and not release readiness**.
