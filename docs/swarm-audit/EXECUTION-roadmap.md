# EXECUTION — roadmap2 worker (shipped-feature roadmap grind)

Ownership: extensions/octet-import-pi/**, extensions/octet-import-aider/**, extensions/octet-import-cline/**,
crates/octet-coding-agent/src/tui/{theme,theme_schema,theme_reload,mod}.rs, docs/themes.md,
crates/octet-agent/src/artifact.rs + durable store, crates/octet-coding-agent/src/commands.rs (/fast only),
apps/web/** (#65).

Status: STARTED.

---

# EXECUTION — roadmap3 worker (roadmap remainder + repo-tooling parity)

Ownership: extensions/octet-import-pi/**, extensions/octet-import-aider/**,
extensions/octet-import-cline/**, crates/octet-coding-agent/src/tui/{theme,theme_schema,theme_reload,mod}.rs,
docs/themes.md, crates/octet-agent/src/artifact.rs, docs/parity/repo-tooling.md,
docs/swarm-audit/EXECUTION-roadmap.md, AGENTS.md, NEW docs pages for parity row 6.1.

Rows: #175, #418, #416, #419, parity 6.1/6.2/6.3, #156, #343, #264/#265, #267.

Status: STARTED. Adopted roadmap2 partial work: extensions/octet-import-cline/ (1-line
diagnostic + tests) and extensions/octet-import-pi/ (untracked package, complete on disk).

## 2026-09-15 roadmap3 start
- Read docs/swarm-audit/EXECUTION-roadmap.md, WORK-QUEUE.md, docs/parity/README.md.
- `git diff --stat` adopted paths: cline (2 files, +35), octet-import-pi untracked (5 files).
- No changes yet beyond this evidence header.

## 2026-09-15 #416 + #418 code landed (pre-verification)
- #416: new examples/themes/octet-default.toml (schema-complete variant reference) +
  examples/themes/README.md. Test `shipped_reference_theme_is_schema_valid_and_variant_aware`
  in crates/octet-coding-agent/src/tui/theme.rs parses + compiles the file through
  load_resolved_theme_for and asserts dark/light md_code_bg variant override.
- #418: crates/octet-coding-agent/src/tui/mod.rs now declares `pub mod theme_reload;`
  (was outside the build). Added ThemeFileReload production coordinator + classify_reload_failure
  in tui/theme.rs. Unit tests appended to tui/theme_reload.rs (11 tests) and an end-to-end
  poll test `active_theme_reload_poll_applies_edits_and_retains_last_good` in theme.rs.
- Command: `cargo check -p octet-coding-agent --lib` (next).

## 2026-09-15 BLOCKER: crate build broken outside my paths
- Command: `cargo check -p octet-coding-agent --lib`
- Observed: `error: future cannot be sent between threads safely` in
  crates/octet-agent/src/telemetry/testing.rs:135 (`dyn TelemetryAdapterFixture` not Sync).
  octet-agent fails to build, so octet-coding-agent (and my theme tests) cannot be compiled
  or run until the octet-agent owner fixes telemetry/testing.rs. Not my path.

## 2026-09-15 #419 + parity 6.1 + 6.2 + 6.3 landed (verified where possible)
- #419: added `SEMANTIC_ROLE_VOCABULARY` (pub const) + `published_semantic_role_vocabulary_is_closed_and_accepted`
  test in tui/theme.rs; docs/themes.md now publishes the role vocabulary, the variant
  reference, and the extension-namespaced role/theme channel (and records that a
  manifest-level `contributes.themes` channel is not implemented).
- #416 docs: docs/themes.md variant-reference section + corrected the stale
  "no theme loader" sentence (named file themes load at startup via --theme/OCTET_THEME;
  /theme still accepts only auto/light/dark).
- 6.1: new docs pages docs/session-format.md, docs/packages.md, docs/shell-aliases.md,
  docs/tmux.md, docs/termux.md, docs/windows.md; linked from docs/README.md. settings/
  keybindings/compaction/templates/providers/terminal keep their existing owning pages
  (docs/providers.md NOT edited).
- 6.2: AGENTS.md (repo conventions) + docs/maintainers/README.md + prompts
  {cl,is,pr,wr}.md + skills {release,add-provider,interactive-testing}/SKILL.md.
- 6.3: scripts/diff-model-catalog.py + scripts/test_diff_model_catalog.py.
  Verified: `python3 scripts/test_diff_model_catalog.py` -> "Ran 10 tests ... OK";
  `python3 scripts/diff-model-catalog.py --check` -> "no catalog differences", exit 0.
- #156 verified: `OCTET_PI_IMPORT_TEST_BINARY=$PWD/target/debug/octet python3 -m unittest
  discover -s extensions/octet-import-pi/tests -p 'test_*.py'` -> "Ran 4 tests ... OK";
  cline 11 OK; aider 8 OK. Adopted roadmap2's cline diagnostic+test; octet-import-pi
  package is complete on disk (untracked, needs staging by the user).

## 2026-09-15 #343 landed + VERIFIED; #175 already landed by tui3 (recorded)
- #343: crates/octet-agent/src/artifact.rs — added `StoreRoot::{Temporary,Durable}`,
  `ArtifactStore::with_root` / `with_root_and_limits` / `root()`, and
  `ArtifactError::InvalidStoreRoot`. Durable root is created owner-only, survives the
  store, and only generation scratch trees are removed.
  Verified: `cargo test -p octet-agent --lib artifact::` -> "14 passed; 0 failed"
  (includes the 3 new tests durable_root_*, temporary_store_reports_*).
- #175: NOT landed by me. crates/octet-coding-agent/src/commands.rs (tui3's path) already
  has `Command::Fast(Option<bool>)` at :27, SLASH_COMMANDS "fast" at :182, and the parse
  arms at :487. The crate currently fails to compile only because modes/interactive.rs:4042
  (also tui3's path) lacks the `Command::Fast(_)` match arm. Recorded, not touched.
- Re-verified octet-agent builds now (telemetry/testing.rs break is fixed by its owner).
  octet-coding-agent still fails on other owners' files (keymap.rs FocusGained/FocusLost
  exhaustiveness, interactive.rs:339/:1703, interactive.rs:4042), so the theme tests
  below remain unrun.

## 2026-09-15 BLOCKED rows — exact test + exact blocking ownership
None of #264/#265/#267 can be closed from my paths. Records:

### #264 bounded tool-progress presentation enrichment (blocked)
- Primitive exists: `ToolProgressDecoration::new` (`crates/octet-agent/src/tool.rs:276`, bounded
  by `MAX_PROGRESS_DECORATION_LABEL_BYTES`), reached from
  `ExtensionProgressEvent::Decoration` handling gated by `EXTENSION_FEATURE_PROGRESS_DECORATION`
  (`crates/octet-agent/src/extension_process.rs:14445`). SDK emitter:
  `Extension.progress_decoration(label, detail)` (`sdk/python/octet_extension/extension.py:1161`).
- Test I would add: a fixture tool in `crates/octet-agent/tests/fixtures/extension_hooks.py`
  that calls `progress_decoration("indexing", "3/9 files")` mid-call, plus an integration
  assertion in `crates/octet-agent/tests/extension_hooks.rs` that the recorded sink receives
  exactly one `ToolProgress::Decoration` with the bounded label/detail, that a second decoration
  replaces the first, and that an over-long or control-char label is rejected while the tool
  result still completes.
- Blocking ownership: `crates/octet-agent/src/extension_process.rs` and
  `crates/octet-agent/src/tool.rs` (extension-host worker), plus
  `crates/octet-agent/tests/fixtures/extension_hooks.py` and `crates/octet-agent/tests/extension_hooks.rs`.

### #265 namespaced pre-persistence turn-metadata enrichment (blocked)
- Primitive exists: `PersistenceMetadataHook::before_assistant_persist`
  (`crates/octet-agent/src/extension.rs:229`), collected by `collect_persistence_metadata`
  (`crates/octet-agent/src/agent.rs:3209`), namespaced/bounded by
  `is_valid_extension_metadata_namespace` + `MAX_EXTENSION_ENTRY_METADATA_VALUE_BYTES`
  (`crates/octet-agent/src/session.rs:251`).
- Test I would add: register one hook returning `{"example.turn": {"state": "ok"}}` and one
  returning an invalid namespace / oversized value, run one assistant turn, then assert the
  persisted `Entry` carries only the valid namespaced value, that public metadata projection
  excludes it unless marked public, and that the session JSONL line is written exactly once
  (pre-persistence, not a post-hoc rewrite).
- Blocking ownership: `crates/octet-agent/src/agent.rs`, `crates/octet-agent/src/extension.rs`,
  `crates/octet-agent/src/session.rs` (agent/session worker), plus a new
  `crates/octet-agent/tests/` target.

### #267 typed PostMutation rescan hook (blocked)
- State on disk: `octet_extensions::take_post_mutation_rescans`
  (`crates/octet-coding-agent/src/extensions.rs:3746`) is no longer `#[expect(dead_code)]` because
  `rescan_post_mutation_resources` (`:3754`) drains it, but **nothing calls
  `rescan_post_mutation_resources`**, so there is still no product drain path and no end-to-end
  test. The typed producer side is complete:
  `ExtensionPostMutationDisposition::RequestRescan` → `PostMutationDisposition::request_rescan`
  (`crates/octet-agent/src/extension_process.rs:5918`).
- Test I would add: an extension fixture returning `request_rescan([...])` from `post_mutation`,
  then a product-path test asserting `rescan_post_mutation_resources` is invoked after the
  mutation commit, that a rescan for a stale generation or a stopped process is dropped with a
  bounded diagnostic, and that a rescan never activates a changed source implicitly.
- Blocking ownership: `crates/octet-coding-agent/src/extensions.rs` (extension worker) — the
  caller must be added to the product mutation path there; `take_post_mutation_rescans` itself
  is a public method in that file, not mine.

## 2026-09-15 final verification + link/inventory fixes
- Added extensions/octet-import-cline/README.md (the package had none, unlike aider/pi)
  and fixed 4 relative links across docs/packages.md + the two maintainer skills.
  Verified with an inline link checker over all 21 touched docs: "missing links: 0".
- `python3 -m py_compile scripts/diff-model-catalog.py scripts/test_diff_model_catalog.py` -> ok.
- Reference theme schema conformance (independent of the broken crate): a Python check that
  mirrors theme_schema.rs section/role/surface/layout/glyph validation over
  examples/themes/octet-default.toml printed
  "ok: TOML valid, sections/roles/surfaces/layout/glyphs conform, variants overlay applies"
  (5938 bytes; dark md_code_bg #202630 vs light #f1f5f4). The authoritative Rust test
  `shipped_reference_theme_is_schema_valid_and_variant_aware` is still unrun.
- Extension suites re-run after the README addition: cline 11 OK, aider 8 OK, pi 4 OK.
- Suggested follow-up outside my paths: add `python3 scripts/test_diff_model_catalog.py` to
  the script-test block in .github/workflows/ci.yml (~line 49-50) so 6.3 runs in CI.

## 2026-09-15 #416/#418/#419 VERIFIED (crate now builds)
The other workers fixed keymap.rs/interactive.rs/commands.rs, so
`cargo test -p octet-coding-agent --lib tui::theme` now runs:
- "test result: ok. 50 passed; 0 failed" (1262 filtered)
- includes `shipped_reference_theme_is_schema_valid_and_variant_aware` ok (#416),
  `active_theme_reload_poll_applies_edits_and_retains_last_good` ok (#418),
  `published_semantic_role_vocabulary_is_closed_and_accepted` ok (#419), and all 11
  `tui::theme_reload::tests::*` ok (#418).
- Also: `cargo test -p octet-agent --lib artifact::` -> "14 passed; 0 failed" (#343).
- rustfmt --check is clean on artifact.rs, theme.rs, theme_reload.rs.
