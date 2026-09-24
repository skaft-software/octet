# Browser and computer use: one code tool, host-owned approvals

**Status:** proposed design, revision 2. Not an implementation authorization;
#345's sequencing (stability → Pi compatibility → full computer use) still
holds. References: Codex `~/github/openai/codex` @ `44b857c00` (2026-09-22),
Pi `~/github/earendil-works/pi` @ `890f92088` (2026-09-21).

This replaces the earlier multi-backend/broker proposal. Revision 2 corrects
what Codex and Pi actually ship and resolves contradictions in revision 1.

## Problem

1. `octet-browse` runs a visible Chromium with a **persistent profile owned only
   by octet** (`extensions/octet-browse/REFERENCE.md:13,180`). Logins made there
   survive restarts, but the user's existing Chrome/Edge sessions are not
   reachable, and Browse never pairs with or copies a normal profile.
2. Human co-driving is fragile. `eN` refs die on every new snapshot/navigation,
   so a user touching the window invalidates addressing; every action is a
   model round trip; #377 tracks focus/flicker.
3. Seventeen one-action tools and no composition mean one model turn per
   element. That loop, not the browser, is the slowness.

## What the references actually do

### Codex: two separate subsystems

**A. Code mode** (`codex-rs/code-mode-*`). The `exec` tool runs JavaScript as an
async module in a fresh V8 isolate: "no Node, no file system, no network
access, no console" (`code-mode-protocol/src/description.rs`,
`EXEC_DESCRIPTION_TEMPLATE`). Enabled tools are on a global `tools` object.
Pragma `// @exec: {"yield_time_ms", "max_output_tokens"}` defaults to 10000 ms
(`DEFAULT_EXEC_YIELD_TIME_MS`, `code-mode-protocol/src/runtime.rs:15`) and 10000
tokens. Helpers: `exit`, `text`, `image`, `audio`, `store/load`, `notify`,
`setTimeout/clearTimeout`, `ALL_TOOLS`, `yield_control`. `wait(cell_id,
yield_time_ms, max_tokens, terminate)` resumes a running cell. The isolate runs
in a standalone host process (`Feature::CodeModeHost`, stable). Nested calls go
through normal tool dispatch (`core/src/tools/code_mode/delegate.rs`), so each
keeps its own approvals. `code_mode` itself is `UnderDevelopment`, off by
default (`features/src/lib.rs:1054`), and can hide direct tools
(`CodeModeOnly`) behind `ALL_TOOLS` search.

**B. Browser and computer use.** These do not run through code mode. They are
bundled plugins (`browser@`, `chrome@`, `computer-use@`,
`unified-computer-use@openai-bundled`, in `plugin/src/bundled_hooks.rs`). Each
runs an MCP server, `node_repl` or `cua_repl`, that exposes a `js` tool
(`protocol/src/mcp.rs:39`, `ext/guardian-v2/src/async_scorer/observation.rs:103`).
Cleanup runs through `turn_ended` hooks on Stop/Interrupt/SubagentStop. The
browser runtime itself is not in the repository. Inside `tools/call`, the
server asks the host for per-action approval
(`app-server/tests/suite/v2/mcp_tool.rs:1357-1375`).

**Codex policy has three layers, and only one is typed:**

- **Typed allow/deny ceilings.** User config (`config/src/browser_use.rs`,
  `computer_use.rs`, `deny_unknown_fields`) contains only:
  - `allow_history_access`
  - per-origin `access`, `downloads`, `uploads`, `full_cdp_access`
  - `default_app_access`, macOS `bundle_ids`, Windows `aumids`/`exes`

  The admin requirements layer (`browser_computer_use_requirements.rs`) also
  has `auto_review`, `persistent_approval`, `access_approval_lifetime`
  (turn|thread), `allow_webmcp`, `disable_auto_review`,
  `allow_global_persistent_approval`, `allow_locked_computer_use`, and
  `allow_persistent_approval`.
- **Kill switches, not opt-in gates.** Feature flags `BrowserUse`,
  `BrowserUseFullCdpAccess`, `BrowserUseExternal`, and `ComputerUse` are
  "requirements-only", stable, and enabled by default.
- **Prompt-text judgment.** Confirmation policies come from per-model metadata
  (`core/src/mcp_tool_call.rs:1366-1390`). Guardian auto-review is itself a
  model call that follows `prompts/templates/guardian/node_repl_policy.md`:
  - evaluate nested calls recursively;
  - treat every site as untrusted;
  - judge clicks by the actual interface state;
  - treat consequential effects as high risk.

### Pi: no new model surface at all

Pi takes a clear position: "No MCP … build CLI tools with READMEs", "No
permission popups", no in-process sandbox that pretends to be one
(`packages/coding-agent/README.md:501-513`, `docs/security.md`). Its browser
answer is the `pi-skills/browser-tools` skill: bash scripts over CDP on `:9222`.
Its `--profile` flag copies the user's profile to keep logins. It fixes
per-element slowness by running batched JS in the page (`browser-eval.js`).

### What octet takes from each

| Decision | Source | octet choice |
| --- | --- | --- |
| Composition | Codex code mode (A) | Host `exec`/`wait` with Codex's contract |
| Browser/computer runtime | Codex plugins (B) shape | Extensions publish ordinary tools; nested inside `exec`; turn-end cleanup |
| Typed policy | Codex user-config ceilings only | allow/deny tables; no requirements layer yet |
| Judgment policy | Codex Guardian (LLM) | **Not copied.** Deterministic host classification (#383) |
| Honesty | Pi | No sandbox claims without OS containment; no page-JS eval by default; no profile copy |

Choosing deterministic classification over an LLM reviewer is a deliberate
departure from Codex. It makes approval reuse (below) a design requirement.

## Design

### 1. `exec`/`wait` (host tool layer)

- **Model surface.** These two tools are the only new model-facing surface.
  They live in the host tool layer because they must nest every enabled tool,
  including extension tools. Semantics, helper names, pragma, and defaults copy
  Codex code mode exactly.
- **Nested tools.** Nested tools keep their names (`tools.browser_click(...)`).
  Each nested call goes through the same dispatch path as a direct call:
  policy evaluation, approvals, result persistence, and untrusted-content
  marking. One approval never covers a whole program.
- **Visibility.** A new per-tool catalog attribute, `nested_only`, hides a tool
  from the direct model list while keeping it callable and listed in
  `ALL_TOOLS`. Browse sets it on action tools. Status/launch/close stay direct
  for the `/browse` UX.
- **Engine and boundary: an explicit #391 decision, not an implementation
  detail.**
  - The engine is embedded V8 (`v8`/rusty_v8), run in a separate octet child
    process like Codex's `CodeModeHost`, with no ambient bindings beyond the
    helpers.
  - QuickJS is the fallback only if V8 binary size or build cost fails the #390
    packaging gate. Python is not an option: it cannot provide an isolate.
  - The existing `code_runtime.py` AST interpreter is retired, not reused.
- **Containment, stated honestly.** The isolate is the security claim for
  *model code*. Nested tools still run with their normal authority. This
  reverses `code_runtime.py`'s fail-closed "no execution without OS
  containment" stance. It is acceptable because model code has no ambient
  authority and every effect is a host-dispatched, policy-checked tool call.
  Calling that a "sandbox" is still forbidden (#391).
- **Budgets per cell** (proposed, tuned in #391):
  - 64 KiB source;
  - `max_output_tokens` as in Codex;
  - V8 heap limit;
  - wall-clock deadline, with `terminate` interrupting via the isolate;
  - 256 nested calls;
  - bounded timers.

  Python AST node and depth caps do not carry over.

### 2. Approvals: one seam, binary config, bounded reuse

- **Config stays allow|deny.** "Ask" is not a config value. It is a result of
  the host's classification of an allowed action, using the existing
  `extension_policy.rs` adapter model, where hints can only increase caution.
  That keeps the "no tri-state enums" non-goal consistent.
- **Action classes:**
  - **Observe:** snapshot, screenshot, read, scroll, wait. Allowed inside an
    allowed origin/app. No approval.
  - **Local edit:** typing into an unsubmitted field, navigating within an
    allowed origin. Allowed. No approval unless adapter hints raise it.
  - **Consequential:** submit, send, purchase, delete, permission change,
    download/upload, or cross-origin navigation carrying typed data. These need
    approval.
- **Binding.** A consequential approval binds operation + target (tab/origin
  or app/window) + argument digest + owner triple + the observation digest the
  user saw. It is single-use, which is correct for consequential effects:
  approve what is on screen, never what the model says it intends.
- **Origin/app access grants** are a separate class of approval, and the only
  reusable one. The user grants "use `https://example.com`" or "use Safari"
  once per turn, up to a host-fixed cap. That removes per-click prompts for
  the first two classes.
  - Grants and approvals are scoped to the `exec` call's tool-call ID, so
    nested calls in one cell and later `wait` resumes share them.
  - When the existing 5-minute `MAX_EXTENSION_APPROVAL_TTL` expires, the host
    asks again; it is not extended for this work.
  - Thread-lifetime and persistent approvals are out of scope until a
    requirements layer exists.
- **Suspension.** An approval prompt suspends the cell, and `exec`/`wait`
  yields `Script running with cell ID ...`. If the approval is denied or
  expires, the nested call throws a typed error in JS. There is no replay after
  ambiguous effects.

### 3. Browser (`octet-browse`)

- **Tools.** The 17 tools become `nested_only` action tools plus the direct
  lifecycle tools. The implementation stays Playwright (pinned `1.57.0`).
- **Addressing inside `exec`.** Nested browser calls use semantic locators
  (`role=button[name="..."]`, `text=...`) resolved at action time. `eN` refs
  stay ephemeral and fail closed when stale. Because a locator is re-resolved
  from the live page, user interaction in the window does not poison it.
  This closes the addressing half of problem 2.
- **Logins, three options, weakest-claim first:**
  1. **Default:** the existing isolated persistent profile. The user signs in
     manually once, and it stays signed in. No new code.
  2. **Opt-in existing Chrome/Edge:** through the host connector seam that
     already exists (`extensions/octet-browse/CONNECTORS.md`), with a
     Playwright existing-session bridge as the connector. This requires the
     user to install Microsoft's Playwright bridge browser extension.
     - That is a **third-party** companion extension. The non-goal below
       covers only first-party ones.
     - Document that Chrome 136+ refuses remote debugging on the default user
       data directory. Direct CDP to the user's real profile is not a
       supported path.
  3. **Rejected:** Pi's profile copy (it copies cookies/credentials out of the
     browser's store).
- **No page JavaScript eval** by default. Pi-style `browser-eval` is
  `full_cdp_access`-class power. It stays denied unless an origin policy
  allows it, and it is not part of this work.
- **Cleanup.** A turn end, interrupt, or stop releases input and closes
  targets. This mirrors Codex's `turn_ended` hook.

### 4. Computer use (`extensions/octet-computer-use`)

The extension keeps its lifecycle/policy/containment design and gains a real
backend (#385 macOS, #389 Windows). Its `observe`/`act` become nested tools of
`exec`, and its private code transport is removed. App policy uses the
`[computer_use]` tables.

### 5. Config (`~/.octet/config.toml`)

These are Codex's user-config fields only. Unknown keys are rejected:

```toml
[browser_use]
allow_history_access = false

[browser_use.default_origin_policy]
access = "allow"
downloads = "deny"
uploads = "deny"
full_cdp_access = "deny"

[browser_use.origins."https://bank.example"]
access = "deny"

[computer_use]
default_app_access = "deny"

[computer_use.macos.bundle_ids]
"com.apple.Safari" = "allow"

[computer_use.windows.aumids]
"Microsoft.WindowsCalculator_8wekyb3d8bbwe!App" = "allow"
```

Approval lifetimes are fixed by the host (§2), not configurable.

### 6. Human usability and untrusted content

- **Usability.** #377 (focus/flicker) plus stop/takeover and input release as
  host verbs the model cannot call (#386). Locator re-resolution (§3) covers
  addressing.
- **Untrusted content.** Page text, AX trees, screenshots, and tool results are
  data and never grant permission. Guardian's rules (judge by the actual
  interface, all sites untrusted, consequential effects high risk) are encoded
  as host classification and observation binding, not as prompt text.

## Alternative considered: `browser_run` inside `octet-browse`

A Pi-shaped option: one extension tool that runs a bounded script against a
page-scoped Playwright API, with no kernel change.

- **Advantages:** cheaper, no embedded JS engine.
- **Disadvantages:**
  - browser-only, so computer use and other tools cannot be composed;
  - the extension would need its own isolate, and Python cannot provide one;
  - approvals would become a nested second seam.

Kept only as the fallback if the #391 engine gate fails.

## Explicit non-goals

- No requirements/admin layer, thread-lifetime or persistent approvals, or
  model-catalog confirmation policies.
- No tri-state config enums, receipts ledger, or new host services beyond #383
  and the `nested_only` attribute.
- No first-party companion browser extension, native-messaging host, or
  profile/cookie import.
- No page-JS eval, raw CDP, pixel-coordinate fallback, hover/drag breadth, or
  multi-window orchestration beyond what a journey needs.
- No Sitegeist-style skills library, artifact apps, or overlay UX.
- No replay after ambiguous effects; no relabeling clicks as read-only.

## Plan

| Slice | Leaf | Deliverable |
| --- | --- | --- |
| 1 | #391 | Engine/boundary decision record; `exec`/`wait` + V8 child process, budgets, `nested_only`, escape tests |
| 1 | #383 | Action classes, consequential approvals bound to observation, per-turn origin/app grants, cell-scoped reuse |
| 1 | #377 | Focus/flicker fix (independent UX gate) |
| 2 | #384 | Browse as nested tools, locator addressing, persistent-profile default, opt-in connector bridge |
| 2 | #386 | Persistent targets, cancellation, turn-end cleanup, input release, stop/takeover |
| 2 | #387 | Bounded screenshot references via the artifacts service |
| 3 | #385/#378 | macOS desktop backend; Firefox/Safari journey (documented as weaker) |
| 3 | #389/#388 | Windows backend; native Responses `computer_call` transport |
| 4 | #390 | Pinned packaging (V8 size gate), real-task comparison vs pinned Codex, security review |

## Verification

**Hermetic tests first:**

- exec/wait cell semantics, yield and `terminate`;
- `nested_only` visibility and `ALL_TOOLS`;
- isolate escape attempts: imports, filesystem, network, subprocess, host
  objects, infinite loops, heap exhaustion;
- budget exhaustion;
- approval matrices: classes × allow/deny config × grant/expiry/reuse within a
  cell and across `wait`, denial surfacing as a JS error, observation-digest
  mismatch rejection;
- stale-ref fail-closed vs locator re-resolution after a simulated user
  interaction;
- stop/takeover.

**Then real-backend journeys:** one per leaf on the pinned candidate, including
a signed-in task in the persistent profile. Each is compared against a pinned
Codex setup (#345), with prompt counts and turns-per-task reported honestly.

**Security gate:** a security review of the permission delta gates enabling
real backends.
