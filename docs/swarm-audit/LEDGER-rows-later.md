# rows-later bundle ledger (4b serve/companions, 4d benchmarks, 4e research spikes)

#58 | implemented | apps/web/src/shortcuts.ts:24 GLOBAL_SHORTCUTS table; apps/web/src/App.tsx:326 "?" shortcut-reference panel; apps/web/src/shortcuts.test.ts | —
#65 | partial | apps/web/src/styles.css:182-215 .app-shell grid (sidebar/center/activity-inspector); apps/web/src/App.tsx:1215 resizePaneBy persists widths; apps/web/src/App.tsx:2089 pane resize handle | fixed pane set only: no user-created splits or rearrangeable layout
#70 | implemented | apps/web/src/model-preferences.ts:1-60 bounded per-model reasoning store; apps/web/src/model-preferences.test.ts | —
#71 | implemented | crates/octet-coding-agent/src/extensions/serve/runs.rs:39-56 PR store+refresh worker; apps/web/src/notifications.ts:70 PR notification copy; apps/web/src/notifications.test.ts | —
#127 | not-code | BACKLOG.md:169 design review; no design doc under docs/design/ for Serve command center | human design review, no code artifact committed
#180 | partial | crates/octet-coding-agent/src/cli.rs:133 Serve variant has only no_open/port/web_root; serve/startup.rs:64 run_with_session_name; serve/startup/tests.rs:14 | no CLI flag reaches it: lib.rs:147 dispatch calls run() which passes None
#221 | partial | apps/ios/project.yml:17 names Sources/OctetCompanionApp.swift (absent from tree); apps/ios/Package.swift:19 test-target path absent; apps/ios/Sources/OctetCompanion/ has only 7 shared-service files | no @main app entry, no Tests/OctetCompanionTests, no UI/views
#341 | partial | docs/qualification/serve-lifecycle-current-candidate.md:3 scopes #396+#341; extensions/octet-serve/tests/security_full.rs:29 opaque session-scoped resources; scripts/check-octet-serve-boundaries.sh:1 diff gate | candidate revision "pending; source-only and uncommitted"; no recorded cross-boundary invariant run
#382 | partial | docs/issue-evidence/382/v0.7.4-qualification.md:38-47 physical gates all UNRUN; tui/theme.rs:2514 ansi256_diff_surfaces test; tui/theme.rs:2785 fixed-palette test | Terminal.app/Ghostty/Ubuntu-SSH visual cells never run
#394 | partial | apps/macos/Sources/OctetMacOS/OctetMacOSApp.swift:3 @main + views; apps/macos/Tests/OctetMacOSTests/AppPoliciesTests.swift:4; apps/macos/README.md:32 | source-only: no build/sign/notarize/live run; tests cover notification/backoff policies only
#395 | absent | docs/qualification/serve-backend-lifecycle-full.md:34 independent checks "intentionally NOT run"; docs/experimental/octet-serve/web-acceptance.md:3 criteria, "not a current test result" | no independent Serve+companion audit artifact exists
#396 | partial | docs/qualification/serve-lifecycle-current-candidate.md:7; extensions/octet-serve/tests/lifecycle_full.rs:126 attach/owner fencing; lifecycle_full.rs:200 stale-generation + cursor-bound replay | candidate uncommitted; no reconnect/live acceptance record
#4 | absent | crates/octet-coding-agent/src/migrate.rs:84 import subcommand (settings/model/skills/MCP only); docs/pi-migration.md:47 tablet of portable data | no Pi session/transcript import path
#23 | not-code | docs/design/extensions-spike.md:177 LSP row "No host LSP manager" | spike/evaluation deliverable; no LSP layer or design doc committed
#42 | absent | crates/octet-coding-agent/src/migrate.rs:2404 only analyzes Pi eventBus usage; docs/pi-migration.md:211 local event bus is a Pi-bridge compat feature | no host-level extension-to-extension event bus or API
#119 | not-code | docs/design/extension-capability-and-orchestration-boundaries.md:91 scopes #119 as a spike with stop criteria | no durable-graph design or evaluation artifact committed
#150 | implemented | crates/octet-coding-agent/src/presentation/request.rs:187 tokens_per_second_milli; presentation/request.rs:203 RequestThroughputTracker; presentation/ownership_tests.rs:131 | —
#174 | absent | Cargo.toml:14 edition = "2021"; crates/sexy-tui-rs/Cargo.toml:4 | no crate is on Rust 2024
#175 | absent | crates/octet-coding-agent/src/commands.rs:135 SLASH_COMMANDS (30 entries, none "fast") | no /fast command or Codex-only gating
#184 | not-code | BACKLOG.md:205 spike scope; docs/experimental/octet-serve/current-state.md:388 fixture-only/absent list | no background-session evaluation artifact
#191 | partial | docs/benchmarks/README.md:92 bench-systems.py method; docs/benchmarks/runtime-footprint-2026-08-29.md:1 peak-RSS medians; docs/benchmarks/README.md:17 historical only | no 0.7.x RSS campaign; PSS unavailable on that host
#192 | partial | docs/benchmarks/tb21-v0.6.2/README.md:34 frozen Ygg 0.6.2; docs/benchmarks/README.md:25 Harbor adapter reproduces 0.6.2 only | TB2.1 not re-run on current release; no Harbor Index campaign
#194 | absent | docs/benchmarks/README.md:15-23 lists every published campaign (no TB4); BACKLOG.md:192 | no Terminal-Bench 4 harness wiring or campaign evidence
#195 | not-code | docs/benchmarks/beta-protocol.md:11-13 protocol stub; docs/benchmarks/README.md:204 report method | no voluntary install/daily-driver reports recorded
#219 | partial | docs/benchmarks/README.md:46 telemetry records context occupancy; docs/context.md:21 per-turn provider-visible request estimate | no baseline or per-integration context-footprint measurement published
#347 | not-code | BACKLOG.md:206 effort Epic / Human decision; docs/design/extensions-spike.md:175-178 Claude Code comparison rows | separately scheduled human follow-up; no audit artifact
#348 | not-code | crates/octet-coding-agent/src/providers/declarations.json:66 anthropic_models discovery kind; docs/qualification/discovery-current-candidate.md:3 verification UNRUN | no reconciliation artifact for verified Anthropic discovery vs limits

NOTES:
- apps/web has no user-defined splits: the "panes" are a fixed 3-region shell (styles.css:182-215) with resizable, persisted activity/inspector/terminal widths (App.tsx:1215, 2089) and a Playwright test at tests/workspace.spec.ts:1144. #65 is near-complete, not absent.
- apps/ios is missing files its own manifests reference: project.yml:17 Sources/OctetCompanionApp.swift and project.yml:38 / Package.swift:19 Tests/OctetCompanionTests. Swift build/test of that package cannot succeed as committed; apps/macos is complete at source level.
- Serve qualification records self-declare "source-only", "pending", "unverified" (serve-lifecycle-current-candidate.md:7; serve-backend-lifecycle-full.md:34). Fixtures exist but no independent audit artifact does, so #395 stays absent.
- Triage-only run: no cargo/npm/swift commands executed; verdicts rest on source and checked-in evidence reads.
