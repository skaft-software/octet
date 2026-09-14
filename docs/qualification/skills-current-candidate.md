# Skills current-release qualification candidate

**Status:** source-backed documentation candidate only. No tests, builds, fixtures, or
live runs were executed in this coding phase. This change does not alter production
source, APIs, configuration, or tool allowlists.

## Truthful current contract

- The first-party model-visible core registry is `read`, `edit`, `write`, `bash`,
  and opt-in `search`. Skills are filesystem resources; they do not add a
  model-facing skill tool.
- Discovery adds a bounded catalog of skill metadata and `SKILL.md` locations to
  the composed prompt. A model uses the ordinary `read` tool for that file and
  referenced files; the normal allowlist, sandbox, and path policy still apply.
- TUI `/skills load NAME` resolves a skill and prefills `/skill:NAME`. Submission
  expands the body into an ordinary user message and checks declared required
  tools. It does not append a durable activation. TUI `/skills off` can append a
  deactivation only for an activation already present on the branch.
- Serve's slash-command worker appends durable `SkillActivated` on load and
  `SkillDeactivated` on off. That activation path is Serve-specific, not a
  generic TUI guarantee. Plain, print, RPC, and interactive prompt submission
  expand explicit `/skill:NAME` text as ordinary prompt content.
- A TUI inline body is ordinary history and may be summarized by compaction; it
  does not become active session state. Serve activation state is reconstructed
  from session events and compaction snapshots. The session schema has resource
  snapshot fields, but no current registered model tool exposes a resource-read
  API.

Skill bounds are source-defined: YAML frontmatter 32 KiB, `SKILL.md` 256 KiB, and
supporting text reads 512 KiB under `references/` or `templates/`.

## Source evidence

- `crates/octet-agent/src/tools/mod.rs:110-123` registers only the five core
  implementations, with `search` available through an explicit surface.
- `crates/octet-coding-agent/src/app/bootstrap.rs:5578-5585,5777-5784` loads
  `CoreTools`, applies policy, discovers the filesystem skill registry, and
  appends the read-based skill catalog.
  `crates/octet-coding-agent/src/resources.rs:364-428` advertises only
  executable core tools in the base prompt; `1401-1440` emits the `read`/location
  instructions.
- `crates/octet-coding-agent/src/resources.rs:1319-1377` loads `SKILL.md` and
  bounded supporting files; `1443-1477` expands `/skill:` into ordinary prompt
  text and `1016-1032` checks required tools.
- `crates/octet-coding-agent/src/modes/interactive.rs:3481-3627` implements TUI
  `/skills`; `3583-3588` only prefills `/skill:NAME`, while `3604-3625` handles
  deactivation. Expansion is submitted at `5760-5772`. Plain, print, and RPC use
  the same expansion at `crates/octet-coding-agent/src/modes/plain.rs:210-220`,
  `crates/octet-coding-agent/src/modes/print.rs:58-69`, and
  `crates/octet-coding-agent/src/modes/rpc.rs:1090-1101`.
- `crates/octet-coding-agent/src/extensions/serve.rs:5679-5731` is the Serve
  slash-command implementation that appends `SkillActivated`/`SkillDeactivated`.
  The Serve presentation/evidence switches at `7926-7934` and `8943-8970` are
  not tool registration.
- `crates/octet-agent/src/session.rs:477-580` defines activation/resource
  entries; `2147-2193` snapshots active state at compaction; `2776-2885`
  reconstructs it. Existing focused fixtures in
  `crates/octet-coding-agent/src/resources.rs:2291-2318,2423-2485` cover
  required tools and compaction state.

## Remaining gaps

### Code-policy gap

`crates/octet-coding-agent/src/config.rs:118-128,143-157` still accepts the
removed names `search_skills`, `load_skill`, and `read_skill_resource` in
`SUPPORTED_TOOL_NAMES` and the default policy.
`crates/octet-coding-agent/src/app/bootstrap.rs:5544-5566` then rejects a
requested name absent from the final registry, and
`crates/octet-coding-agent/src/app/bootstrap/tests.rs:2812-2828` records that
the legacy load name is not registered. An explicit `--tools` request can
therefore pass policy parsing but fail final startup validation. This docs task
intentionally does not change that source policy or registry.

### Activation gap

`crates/octet-coding-agent/src/modes/interactive.rs:3583-3588` only prefills
`/skill:NAME`; its submit path expands ordinary prompt text at
`crates/octet-coding-agent/src/modes/interactive.rs:5760-5772`,
while only the Serve worker at
`crates/octet-coding-agent/src/extensions/serve.rs:5679-5701` appends
`SkillActivated`. TUI `off` can append only `SkillDeactivated` for pre-existing
state (`crates/octet-coding-agent/src/modes/interactive.rs:3593-3625`).
The session schema supports resource events/snapshots
(`crates/octet-agent/src/session.rs:557-613`), but the current
registry has no model-facing resource tool; Serve's `read_skill_resource` branches
(`crates/octet-coding-agent/src/extensions/serve.rs:7926-7934,8943-8970`) are
presentation/evidence handling, not registration. The documentation now states
these limits rather than implying durable TUI activation or a resource-tool path.

## Proposed UNRUN checks and gates

1. Run the existing bootstrap registration/policy tests and the focused resource
   required-tool/compaction tests above; expected result is no skill-tool schema
   and durable snapshots only for explicit session events.
2. Add a bounded TUI fixture (not written here) that asserts load-prefill,
   submit-time inline expansion, absence of `SkillActivated`, and compaction's
   ordinary-message behavior. Add/retain a Serve fixture for load/off events.
3. Run a Markdown link/anchor check and a search confirming the removed names do
   not occur in model-facing instructions. These are UNRUN.

Dependencies are coordinator integration plus the assigned Rust/non-Rust
verification lanes; no source handoff is requested. Remaining behavioral gates
include real TUI/PTY compaction and resume, Serve transport/session projection,
provider-backed prompt delivery, and installed-binary parity. Physical Windows,
live provider, website/package publication, and long-duration gates were not
assessed here.
