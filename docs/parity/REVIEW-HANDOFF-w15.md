> **Advocacy, not evidence.** This handoff is the wave authors' own argument for their work;
> nothing here is independent proof. The adversarial pass already **falsified attack #7**:
> plain mode still rendered `[completed with warnings]` instead of the TUI's `[completed]`
> family, so the claim was false as written for everything built from `ae0ffee2`. The P0
> dogfood cut repairs plain mode; read §8 as claims to attack, not as results.

# Wave 15 review handoff — octet 0.8.0, Pi parity and live reload

Audience: an adversarial review agent. Everything below is either a verified fact with a
receipt or is explicitly marked as unverified. If a claim here lacks a receipt, treat it as
false until you prove it.

> **STATUS BLOCK — GREEN.** The final wave ran on the frozen snapshot
> `freeze-w15f.sha256` (206,050 files) and every suite exited 0; the snapshot was re-verified
> afterwards with zero drift. §6 holds the per-suite numbers. Everything a reviewer needs to
> falsify is in §8.

## 1. What this branch is

- Repository `skaft-software/octet`, branch `vibe/pi-parity-roadmap-df5a7e80`.
- Baseline at the start of this wave: `79e0fd31`. Earlier: `8ff29b09`, `00ceea5c`, `a4873790`.
- Workspace version `0.8.0`; **SDK/public distribution stays `0.7.6`** by decision.
- Pi reference: read-only `~/github/earendil-works/pi` @ `origin/main` (our pin when the wave
  started was `8a7b0c03`; main has since advanced to `e4c75a732`, and the in-progress rewrite
  lives on the `pico` branch — see §4).
- Mandate: finish the backlog with real evidence, ship a usable 0.8.0, deliver the live-reload
  vision. No pushes, no publications; local commits only.

## 2. How to reproduce

```sh
cd /Users/achumukundan/github/skaft-software/octet
git log --oneline -3 && git status --short | wc -l
python3 /tmp/octet-final/guarded-run.py <name> cargo check --workspace --all-targets --all-features --locked --offline
```

The guarded runner is the only builder on this machine (jobs=2, incremental off, 30-minute cap,
25 GiB disk floor). Logs: `/tmp/octet-final/<name>.log`, exit codes `<name>.exit`.

**Known environment flake, and one deliberate exclusion.** `real_activity_wait_pty_contract`
can wedge a spawned `octet` in an unkillable kernel exit state (`?Es`), blocking
`Child::wait()` forever. It recurred on 2026-09-16 19:29 and was cleared by hand; the wave
excludes it and records that in `/tmp/octet-final/w15-verify.excluded`. It must be run
separately on a clean process table, and no result here may depend on it.

## 3. What changed in this wave, by area

| Area | Workers | Entry points |
| --- | --- | --- |
| Provider/protocol codecs | ai-codecs, ai-runtime, ai-integrate | `crates/octet-ai/src/{protocol,declarations,client,types,images}`, `tests/*_current.rs` |
| Agent kernel | kernel, agent-integrate | `crates/octet-agent/src/{agent.rs,tools/deferred.rs,session.rs,events.rs,telemetry/schema.rs}` |
| Delegation durability | worker-reattach | `crates/octet-agent/src/delegation.rs`, `extensions/octet-subagents/**` |
| Product surfaces | providers, commands, coding-integrate | `crates/octet-coding-agent/src/{providers,app/bootstrap*,commands.rs,modes,session_store.rs}` |
| TUI | tui | `crates/octet-coding-agent/src/tui/**`, `crates/sexy-tui-rs/**` |
| Extensions/SDK | extensions, extensions-cont, live-registration | `crates/octet-agent/src/{extension_process.rs,extension_provider.rs}`, `protocol/extension-api-v0.3*`, `sdk/**`, `extensions/**` |
| Pi bridge | bridge | `crates/octet-coding-agent/src/{pi.rs,pi/**,migrate/**}`, `extensions/octet-pi-compat/**` |
| Live reload | reexec, reexec-multi, reexec-e2e, reload-supervisor | `crates/octet-coding-agent/src/reexec.rs`, `src/reload.rs`, `modes/interactive.rs`, new tests |

## 4. Live reload — the vision and what is actually claimed

The owner's requirement: *"an update while agents are running on tasks should be able to hot
reload everything while it's working… fast iteration like in game dev: extension designs,
skills, users' own jank modifications to the octet agent itself."* Plus: it must work with
several TUI panes and a running `octet serve`.

### 4.1 Pi's actual reload surface (receipts from the Pi reference)

| Pi behaviour | Receipt |
| --- | --- |
| `/reload` reloads extensions, skills, prompts, themes, context files (+ settings, keybindings) | `packages/coding-agent/docs/extensions.md:1320`; CHANGELOG entries for themes/context/keybindings/settings |
| `ctx.reload()` runs the same flow from an extension: `session_shutdown` → resources → `session_start(reason:"reload")` + `resources_discover(reason:"reload")` | `docs/extensions.md:1314-1330` |
| `registerTool` / `registerProvider` / `unregisterProvider` take effect immediately, "removing the need for `/reload` after late registrations" | CHANGELOG ~2839-2874 |
| Watching is narrow: theme files, and git HEAD in the footer at a 1 s `watchFile` interval | `src/core/footer-data-provider.ts:336` |
| No binary re-exec — Bun re-imports modules in process ("full runtime reload") | `ctx.reload` semantics |

### 4.2 What octet does now

- `/reload` already covered instructions, prompts, skills, keybindings and theme; extension
  restart exists with `ExtensionReloadReport { generation, previous_shutdown_graceful }`.
- API 0.3 already declares the `ctx.reload()` equivalent: `session/reload`
  (ExtensionToHost, capability `session_lifecycle`, `SessionReloadParams`), and the session
  shutdown reason vocabulary includes `reload`.
- **Beyond Pi:** the host binary can re-exec. Same PID, same session resumed
  (`--resume <id>`), extension children stopped through the bounded path, session-lock fds
  required to be `CLOEXEC` (otherwise the new image deadlocks re-locking its own session),
  no lock ever taken on the executable so N panes plus serve reload independently.
- **The loop:** a poll-based supervisor marks layers stale (resources, extensions, host) and
  reloads them at the next idle boundary, debounced, in a fixed order, with a per-layer report
  and explicit force/dry-run modes. Its watcher generalises the previously unwired
  `tui/theme_reload.rs` engine.
- **Workers:** live workers are detachable rather than fatal — their durable records are
  flushed before the boundary and reattached afterwards, generation/lease-fenced so a stale
  claimant cannot run the same worker twice.

### 4.3 What a reload still loses (state this in review; it is deliberate)

- A **model call in flight** dies and is not replayed; the record survives.
- An extension's **in-flight host request** is fenced during its restart.
- A worker **mid-call** loses that call; its durable record and session survive and reattach.
- `octet serve` does **not** hot reload itself in this release: it is a separate process and is
  unaffected by a pane reloading, but picking up a new binary needs a restart until a serve
  reload trigger exists.

### 4.4 Verification for the reload claims

- 32 `reexec` unit tests: generation identity, probe round-trip plus malformed/incompatible/
  timeout/spawn-failure, TOCTOU (changed during probe and after preparation), every safety
  refusal with a fixed order, argv canonicalisation for `--continue` / bare and valued
  `--resume` / `--resume=<id>` / `--fork` / plain, extension shutdown called once and blocking
  on failure, exec-failure recovery, CLOEXEC hygiene, pinned notices.
- End-to-end (PTY): probe over the real binary; reload with the on-disk binary replaced, asserting
  **same PID, same `--resume <session>`, new image**; unchanged binary is a no-op; two panes
  where one reloads and the other keeps a healthy session; serve coexistence where hermetic.
- **Not yet proven at the time of writing:** the end-to-end PTY results and the supervisor's
  wiring (the supervisor worker reports hunks for `modes/interactive.rs`, which the parent
  applies). Check §6 before repeating the claim.

## 5. Public interfaces added this wave

- `octet-ai`: deferred responses (`src/deferred.rs`), `Response.deferred`, `AiError::Deferred`,
  image-generation API + `images::ImageModelCatalog`, request overrides.
  **Not landed:** `ToolResult.usage` — durable per-tool usage is an open gap (row 1c.10 Partial).
- `octet-agent`: `tools::deferred::*` (store, records, states, permits, replacement), the
  replaceable JSONL record `{"type":"deferred_run",...}`, `Agent::{suspend,cancel,resume}_deferred_run`,
  `AgentError::{Deferred,DeferredSuspended,DeferredSuspensionRefused}`,
  `events::{DeferredRunSuspended,DeferredRunResumed}`, telemetry span `octet.agent.deferred_run`,
  `ToolOutput::{with_usage,usage}`; delegation durability fields (`claim`, `lease`,
  `lease_refusal`, `session_owner_released`) and reattachment entry points.
- `octet-coding-agent`: hidden `/debug` + `TUI::rendered_frame()` + `RenderCommand::DumpFrame`;
  `reexec::{ReexecController,ReexecPlan,ProbePayload,BinaryGeneration}` and the
  `--internal-reexec-probe` flag; `reload::*` supervisor; `/reload` hook.

## 6. What has actually been verified

Authoritative for pass/fail: `/tmp/octet-final/w15-verify.status` and
`/tmp/octet-final/w15-hotreload.status` — **every line `=0`** on the frozen snapshot
`/tmp/octet-final/freeze-w15f.sha256` (206,050 files; verified after the run: `missing=0 added=0
changed=0`, i.e. nothing changed while it ran).

| Suite | Receipt |
| --- | --- |
| `w15-verify-check` — workspace, all targets, all features | exit 0, 16.4 s |
| `w15-verify-coding` — `octet-coding-agent` | **1630 passed, 0 failed, 1 ignored** |
| `w15-verify-agent` — `octet-agent` | **585 passed, 0 failed, 1 ignored** (plus `agent_run` **156 passed**) |
| `w15-verify-ai` — `octet-ai` | **430 passed, 0 failed** |
| `w15-verify-renderer` — `sexy-tui-rs` | **231 passed, 0 failed** |
| `w15-verify-sdk-python` | OK |
| `w15-verify-bridge` — Pi bridge | OK (3 honest skips: real-runtime) |
| `w15-verify-scripts` | OK |
| `w15-verify-ts` — API 0.3 conformance | 47 fixtures OK |
| `w15-verify-generator` — `generate-extension-api-v03.py --check` | exit 0 |
| `hot-agent-delegation` — reattach, live registration, deferred runs, tool usage | **6 passed, 0 failed** |
| `hot-coding-lib` — supervisor + re-exec units | **1630 passed, 0 failed** |
| `hot-probe` — `--internal-reexec-probe` over the real binary | **2 passed** (one payload, no trace; never after a terminator) |
| `hot-e2e` — PTY re-exec | **3 passed, 1 ignored**: changed binary re-execs in place (same PID, same `--resume <session>`), unchanged binary is a no-op with its notice, one pane reloads while the other keeps a healthy session lock. The ignored one is serve coexistence: a default build has no embedded `serve` feature or installed `octet-serve` package under a scratch`HOME`, so the test names that instead of faking it |

Environment notes that a reviewer must carry forward: `real_activity_wait_pty_contract` is
excluded from the wave (the documented unkillable `?Es` pty wedge; it wedged again during this
work and was cleared by hand — `/tmp/octet-final/w15-verify.excluded`), and
`update::tests::installed_version_probe_*` is load-flaky at `--test-threads=2` under heavy
concurrent load (passes in isolation).

- Ledger of record for the backlog: `docs/swarm-audit/LEDGER-2026-09-16.md` — 124 rows, four
  independent re-audits merged, superseding the 2026-09-15 ledger (which is stale; three of its
  rows were disproven during this wave). Totals: **68 implemented (+11) · 38 partial (−7) ·
  8 absent (−4) · 10 not-code** — 17 verdict changes, 15 escalations and **2 honest downgrades**
  (`#249` Copilot: source plus a fake-origin test only, `docs/providers.md:305` says not closed;
  `#252` pinned provider inventory: the 0.84.4 target was never loaded, three providers remain
  `unsupported`). The four auditors disagreed on no overlapping row. Every coding-side `ok` in
  that ledger is marked provisional because its wave receipts predate the last edits.

## 7. Known limitations and deliberate exclusions

1. **Pi parity is incomplete** — the parity ledger's own counts, not a slogan. The backlog
   re-audit's counts are in `docs/swarm-audit/LEDGER-2026-09-16.md`.
2. **"Declared but not wired"** is the recurring debt pattern the audits found (#418, #343,
   #388, #391, #419, #385, #322, #332, #415 and others): code plus a test target exists with no
   production caller. `LEDGER-2026-09-16.md` has a dedicated section for it — start review there.
3. **Pi's harness is being rewritten on `pico`** (durable tasks, documents, chords; Mario
   Zechner: "it's not a refactor. it's a rewrite… I'll keep the old stuff around until the new
   coding agent materializes on top of the new harness"). Bound compatibility was kept rather
   than expanded; extension-surface parity against the old harness will expire.
4. **In-process workers share the runtime** with the TUI, and the durable session writer does
   `write_all` + `sync_data()` per record (`crates/octet-agent/src/session_writer.rs:83`).
   Measured, documented, not fixed.
5. **Turn ceiling**: the subagents extension caps `max_turns` at 256
   (`extensions/octet-subagents/octet_subagents/model.py:21`) — pre-existing since `afbc731e`.
6. **Withdrawn/excluded by decision**: `ls`/`find`/`grep` tools, `/tree`, `/checkout`, clipboard
   images, rg/fd downloads, chords/CBOR/unix sockets; persisted trust widening and brokered
   OAuth expansion were deliberately out of the parity ledger's scope.
7. **Uncovered gates**: Windows, native SDKs, live provider calls, physical terminals,
   signing/notarization, the Linux-namespace unchanged-source load of all 78 Pi examples.
8. **Serve does not self-reload** (§4.3).

## 8. Attack list (falsifiable claims, hardest first)

1. "An active subagent roster never becomes immutable history and never renders twice."
2. "A deferred poll permit is consumed at most once per generation; a crash cannot yield two
   billable polls."
3. "A re-exec keeps the PID and the exact session, and the new image can re-lock its own session
   file" — run the PTY test, then check `CLOEXEC` on lock fds by hand.
4. "Reload takes no lock on the executable and touches no global state, so N panes and a serve
   reload independently."
5. "Detached workers reattach exactly once, preserving accounting and approval state; a stale
   claimant is refused."
6. "Late `providers/register` takes effect in a running session without a reload, and never
   overrides a built-in the user did not opt into."
7. "`completed with warnings` can no longer reach the TUI, selection, copy text, plain mode or
   the HTML exporter."
8. "Credential redaction covers every new error variant" (`AiError::Deferred`, `AgentError::*`).
9. "JSONL sessions containing `deferred_run` records round-trip through the product's store."
10. "The subagent picker header is the stable surface name and the model column is never
    truncated — even with a long provider-qualified id."
11. "No row in the parity ledger claims more than its artifact proves" — spot-check the ledger
    against the tree; the 2026-09-15 edition failed exactly this test.

## 9. Files to read first

`docs/parity/README.md`, `docs/parity/VERIFICATION.md`, `docs/swarm-audit/LEDGER-2026-09-16.md`,
`docs/commands.md` (the `/reload` row), `CHANGELOG.md`, `docs/releases/v0.8.0.md`, and the
worker reports under `/tmp/octet-final/{tui,extensions,reexec,reexec-multi,reexec-e2e,
reload-supervisor,worker-reattach,live-registration,audit-*,ledger-consolidate}.md`.
