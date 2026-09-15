# TUI parity detail (2a / 2b / 2c / 2d)

Owner document for the `TUI` and `editor`-adjacent rows of
[`docs/parity/README.md`](README.md). Upstream reference (read-only):
`earendil-works/pi` at `8a7b0c03dfb702663acafb6dc29f8acaa4ffe391`. Every claim
below is qualified only by behavior that was actually run; a source- or
type-only check never qualifies a row.

## Not a §2 row, but the maintainer's first pain: slash commands during an active run

`modes/interactive.rs::handle_active_command` renders the observable slash
surfaces **immediately during a live run** instead of queueing them to the next
idle boundary. `ActiveRunInspection::capture(&app)` snapshots the read-only
application facts a run cannot borrow while `Run` holds `&mut Agent`, and
session-scoped reports re-open the same session file with
`Session::open_read_only` — the handle the live `/subagents` drill-in already
uses. Nothing here mutates the running session or the frozen agent.

Immediate during a run: `/help`, `/cost`, `/cache`, `/tree`, `/context`,
`/update`, `/name`, `/export`, `/extensions status`, `/extensions inspect`, the
`/model` inline picker, and the `/thinking` effort picker. The six
"available at the next idle boundary" notices are removed. `/context` during a
run is a plain-text report of the run's own `ContextSnapshot` (the styled
`tui::context::ContextReport` can only be built from `&App`); this is a
deliberate, documented divergence, not a silent one.

### Evidence

`cargo test --locked -p octet-coding-agent --test slash_command_pty -- --nocapture`

```
running 7 tests
test real_octet_slash_enter_invokes_highlighted_command_in_one_submission ... ok
test real_octet_model_picker_selects_and_persists_without_inference ... ok
test real_octet_slash_model_opens_the_picker_while_a_response_is_streaming ... ok
test real_octet_slash_cost_renders_while_a_response_is_streaming ... ok
test real_octet_slash_context_renders_while_a_response_is_streaming ... ok
test real_octet_slash_thinking_opens_the_effort_menu_while_a_response_is_streaming ... ok
test real_octet_slash_help_renders_while_a_response_is_streaming ... ok

test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

The loopback SSE fixture writes only `streaming first chunk`, then holds the
tail until the test releases it. Each streaming test asserts the slash surface
is visible **while** `stream tail complete` is still absent *and* the fixture has
not written its tail, so a queued-to-idle implementation would time out instead
of passing.

## Codex context-window surface in the effort menu

Codex routes only (`Protocol::OpenAiResponses` + `ResponsesRuntimeProfile::Codex`
via `commands::codex_responses_endpoint` — a declared capability, never a
provider name). `/thinking` appends one `Codex context window…` row after the
effort levels; the row is never a level (index-safe through `levels.get`), and
choosing it opens the shared `pickers::codex_context_menu`.

The surface (`commands::CodexContextSurface`):

- reports the **current effective window** taken from the live
  `model.spec.limits.context_window`, i.e. the value the running session actually
  budgets against;
- when the deliberate cap is what reduced the advertised window, prints the typed
  `CodexContextClamp` message with its reason (including that the cap keeps
  octet inside OpenAI's recommended Codex context limit, that usage above 272K is
  double-priced, and that oversized long-running sessions can drop the Codex
  websocket);
- above the 272K standard tier renders cost/usage as **UNCERTAIN** and names
  `Session::record_usage_uncertainty("codex-context-above-272k")` instead of any
  exact-looking figure;
- offers a raise only when the account's plan carries the Pro/ProLite
  entitlement (`ChatGptPlan::uses_max_context_window` read from the same
  subscription credential the launch used; an unreadable credential reports
  `false`). The raise step first names both consequences
  (`CODEX_CONTEXT_ACKNOWLEDGEMENT_WORDING`), then validates through
  `codex_context::resolve_codex_context_window`, so unacknowledged and
  above-entitlement selections fail closed and the deliberate cap stays in force.

The 272K cap is a **deliberate product decision**, not a defect: it is reported
with its reason rather than hidden, and it is only raised through an explicit,
acknowledged, entitlement-checked opt-in.

### Scope limit (honest)

The Codex context window is resolved once at launch from the process environment
(`app/bootstrap.rs`), so no in-session selection can rewrite the live catalog.
The surface therefore reports the exact launch settings that apply a raise
(`--codex-context-window <TOKENS> --codex-context-window-acknowledge-cost-cliff`,
or the equivalent `OCTET_CODEX_CONTEXT_WINDOW` /
`OCTET_CODEX_CONTEXT_WINDOW_ACKNOWLEDGE_COST_CLIFF` pair) instead of claiming an
in-session wire change. `app/bootstrap.rs` is not modified.

## 2c.6 — native text clipboard read with the existing write fallback

Verified for **text only**. `modes/interactive.rs::clipboard_read` follows the
reference helper order (Termux → Wayland → X11 on Linux, `pbpaste` on macOS,
PowerShell `Get-Clipboard -Raw` on Windows); a platform with no declared display
yields no helper at all rather than scraping another transport. Each helper runs
under a 600 ms deadline with `kill_on_drop(true)`, a 1 MiB byte cap and exit-status
checking; every failure path returns `None`, so the existing write transport
(`pbcopy`/OSC 52 in `tui/view.rs`) and terminal bracketed paste remain the
fallback. Reads are inserted through `EditAction::Paste`, so consent, path
attachment and large-paste classification match a terminal paste.

**Clipboard image capture remains excluded** by this document and by the
delivery brief.
