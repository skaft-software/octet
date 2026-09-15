# Pi parity — repo tooling

Detail owner for parity rows `6.1`, `6.2`, `6.3`, and `6.4` in
[docs/parity/README.md](README.md). This page records the authoritative octet
documents and tooling for each row, the evidence, and any remaining gap.

Reference (read-only): `earendil-works/pi` at
`8a7b0c03dfb702663acafb6dc29f8acaa4ffe391`.

## 6.1 — Docs coverage

Every required topic resolves to a real octet page. Where octet already owned the
topic, the existing page is authoritative and this row adds only the missing
pages.

| Topic | Authoritative document | State |
| --- | --- | --- |
| Settings | [docs/configuration.md](../configuration.md) | existing |
| Session format | [docs/session-format.md](../session-format.md) | **new** |
| Keybindings | [docs/commands.md#keys](../commands.md#keys) | existing |
| Compaction | [docs/context.md](../context.md) | existing |
| Templates | [docs/instructions.md](../instructions.md) | existing |
| Providers | [docs/providers.md](../providers.md) | existing (owned elsewhere; not edited) |
| Packages | [docs/packages.md](../packages.md) | **new** |
| Shell aliases | [docs/shell-aliases.md](../shell-aliases.md) | **new** |
| Terminal | [docs/terminal.md](../terminal.md) | existing |
| tmux | [docs/tmux.md](../tmux.md) | **new** |
| Termux | [docs/termux.md](../termux.md) | **new** |
| Windows | [docs/windows.md](../windows.md) | **new** |

New pages are linked from [docs/README.md](../README.md). Two are deliberately
honest gap records rather than compatibility claims:

- `docs/termux.md` records that Android/Termux is unsupported and unqualified
  (no Termux code path, no Android target).
- `docs/windows.md` records the Bash-compatible shell resolution order and notes
  that the `powershell` tool is implemented in the agent runtime but is not yet
  in the coding-agent's model-visible allowlist (parity `4.6`, pending).

## 6.2 — Maintainer prompts, skills, and agent conventions

| Artifact | Location |
| --- | --- |
| Agent conventions | [`docs/maintainers/conventions.md`](../maintainers/conventions.md) (tracked mirror; root `AGENTS.md` is `.gitignore`d local state) |
| Maintainer index | [docs/maintainers/README.md](../maintainers/README.md) |
| Changelog audit prompt | [docs/maintainers/prompts/cl.md](../maintainers/prompts/cl.md) |
| Issue analysis prompt | [docs/maintainers/prompts/is.md](../maintainers/prompts/is.md) |
| PR review prompt | [docs/maintainers/prompts/pr.md](../maintainers/prompts/pr.md) |
| Task wrap-up prompt | [docs/maintainers/prompts/wr.md](../maintainers/prompts/wr.md) |
| Release skill | [docs/maintainers/skills/release/SKILL.md](../maintainers/skills/release/SKILL.md) |
| Add-provider skill | [docs/maintainers/skills/add-provider/SKILL.md](../maintainers/skills/add-provider/SKILL.md) |
| Interactive-testing skill | [docs/maintainers/skills/interactive-testing/SKILL.md](../maintainers/skills/interactive-testing/SKILL.md) |

`.octet/` and the root `AGENTS.md` are gitignored (local agent state), so the
tracked copies under `docs/maintainers/` are the source of truth; maintainers
copy prompts into `.octet/prompts/` and skills into `.octet/skills/`, and read
[`conventions.md`](../maintainers/conventions.md) for the agent conventions. The prompts use only octet's supported
expansion syntax (`${@:-default}`, `${@:start:len}`, `$N`), verified against
`crates/octet-coding-agent/src/prompts.rs:589`.

## 6.3 — HEAD/worktree catalog diff

`scripts/diff-model-catalog.py` diffs the checked-in model catalog between a git
ref (default `HEAD`) and the worktree, using `git show <ref>:<path>` so no branch
switch or checkout is needed. It reports added/removed/changed endpoints and
models and, for every changed model, its **effective reasoning levels** —
mirroring `ReasoningCapability::choices()` (`crates/octet-ai/src/types.rs:480`),
including the effort clamp and the alias resolutions used by
`ReasoningConfig::from_provider_value`.

```sh
python3 scripts/diff-model-catalog.py            # human summary
python3 scripts/diff-model-catalog.py --json     # machine-readable
python3 scripts/diff-model-catalog.py --check    # exit 1 when anything differs
python3 scripts/test_diff_model_catalog.py       # offline regressions
```

Evidence (`2026-09-15`, base `df5a7e80`): `python3 scripts/test_diff_model_catalog.py`
reported `Ran 10 tests ... OK`; `python3 scripts/diff-model-catalog.py --check`
reported `no catalog differences` and exited `0`.

## 6.4 — CHANGELOG release extraction and link repair

Already owned by `scripts/changelog.py` (`parseChangelog`,
`normalizeChangelogLinks`, tag-pinned source links). Not re-implemented here.

## Verification status

### 6.1 / 6.2 observed run (`2026-09-15`, candidate `00e3ca3e`)

- Every topic in the `6.1` table resolves to a real page: a relative-link check over the
  twelve topic pages plus `docs/README.md`, `docs/maintainers/**`, and this page resolved
  **239 relative links with 0 unresolved targets**.
- The maintainer prompts use only expansion syntax supported by
  `crates/octet-coding-agent/src/prompts.rs:589` (`${@:-default}`, `${@:start:len}`, `$N`):
  all four prompts use `${@:-…}` and nothing else.
- The three skills carry `name`/`description` front matter and are 60, 63, and 77 lines.
- Defect found while verifying: the row cited the root `AGENTS.md`, which `.gitignore`
  keeps local-only, so a fresh clone had a broken link and no tracked conventions
  artifact. `docs/maintainers/conventions.md` is now the tracked mirror (root file still
  local), and the references here and in `docs/maintainers/README.md` point at it.
- Defect still open, outside these rows: `scripts/test-packaged-docs.py` requires every
  tracked file under `docs/`, `examples/`, and `sdk/` to be listed in
  `docs/package-assets.txt`, and the tree currently lists far fewer (parity, qualification,
  swarm-audit, examples, and SDK test files are all missing). The `6.1`/`6.2` pages added
  here are inventoried; the rest of the drift belongs to those rows' owners. Reproduce with
  `python3 scripts/test-packaged-docs.py`.
- `scripts/diff-model-catalog.py` and its regressions were run (see 6.3).
- Extension/import tests were run separately (`#156`).
- All new/edited docs were link-checked (21 files, 0 missing relative links).
- `examples/themes/octet-default.toml` passed an independent schema-conformance
  check mirroring `theme_schema.rs` (TOML validity, section/role/surface/layout
  keys, glyph bounds, variant overlay). The authoritative Rust test
  `shipped_reference_theme_is_schema_valid_and_variant_aware` is written but
  unrun: `cargo test -p octet-coding-agent --lib` cannot build because
  `crates/octet-coding-agent/src/tui/keymap.rs` lacks the `InputAction::FocusGained`
  / `FocusLost` variants that `modes/interactive.rs` already matches. Both files
  are owned by other workers.
- Follow-up outside this row's paths: add
  `python3 scripts/test_diff_model_catalog.py` to the script-test block in
  `.github/workflows/ci.yml` so 6.3 runs in CI.
