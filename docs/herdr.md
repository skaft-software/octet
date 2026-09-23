# Herdr integration

[Documentation](README.md) · [Terminal](terminal.md) · [Subagents](subagents.md) · [Sessions](sessions.md)

[Herdr](https://herdr.dev) is a terminal workspace manager for coding agents. It
detects agents running in its panes, rolls their state up to tabs and
workspaces, and drives sidebar badges, notifications, and waits from that state.

octet reports its own lifecycle to Herdr from inside the interactive frontend, so
a pane running octet appears as a first-class agent — the same contract Herdr's
official [Pi, OMP, and Codex integrations](https://herdr.dev/docs/integrations/)
use. It is a Rust rewrite of Pi, so it follows that integration's shape
deliberately: semantic `idle` / `working` / `blocked` state, session identity,
and a strictly increasing report sequence.

**No setup is required.** The integration activates itself inside a Herdr pane
and is a complete no-op everywhere else. There is nothing to install, no hook
file, and no configuration key.

## What appears in Herdr

Inside a Herdr pane, the sidebar and `herdr agent list` show an `octet` agent
whose state follows the live session:

| octet state | Herdr state |
| --- | --- |
| Ready for a prompt | `idle` |
| A prompt was accepted; model or tools are running | `working` |
| Waiting for your approval or tool input | `blocked`, with the on-screen prompt as the message |
| Run settled (completed, interrupted, or failed) | `idle` |
| octet exited | lifecycle authority released |

Because Herdr derives everything else from that semantic state, the ordinary
Herdr workflows apply unchanged: state rollups on tabs and workspaces,
`blocked` attention, completion notifications, and `herdr agent wait` /
`herdr agent prompt` against the pane.

Two deliberate properties:

- **The pane is reported, never the focused pane.** Reports always target the
  pane octet inherited (`HERDR_PANE_ID`), so another client's focus cannot
  redirect them.
- **A failed run settles to `idle`, not `blocked`.** This matches Herdr's
  official Pi integration, which reports `idle` on `agent_settled`; the failure
  itself stays in the transcript.

## What is reported

octet uses the same documented surface Herdr offers every agent
(`pane.report_agent`, `pane.report_agent_session`, `pane.release_agent`), plus one
owner-private record of its own for the restore pass:

- **Semantic state only.** Display customization (pane titles, `$token` sidebar
  values, custom state labels) is left to you through
  `herdr pane report-metadata`, exactly as Herdr documents for user hooks.
- **The session id, not a path.** octet reports its opaque session id — the same
  value `octet --resume <id>` accepts — on the startup report and whenever the
  active session changes (`/resume`, `/new`, `/fork`, `/clone`, or an extension
  `session/switch`). Pi's integration also reports an `agent_session_path`;
  octet deliberately does not, because its transcript path never leaves the
  process.
- **No prompts, tool arguments, credentials, or transcript content.** The only
  free text that leaves octet is the short approval prompt already visible on
  screen, bounded to 200 characters and stripped of control bytes.
- **Ordering by sequence number.** Every report carries a strictly increasing
  `seq` seeded from wall-clock milliseconds, so a restarted octet process in the
  same pane never re-enters the previous process's sequence range and Herdr can
  drop stale reports. Identical consecutive states are not re-sent.

## How it stays out of the way

- **Bounded.** Each report is one socket write with a 500 ms delivery attempt and
  a single 1500 ms retry, the same budget the official Pi integration uses.
  A slow Herdr server can delay a report boundary by up to those attempts;
  reporting errors never fail a prompt, tool, or exit.
- **Silent failure.** Every error is swallowed; there is no retry queue, no
  background thread, and no user-visible diagnostic.
- **TUI only.** The reporter is owned by the interactive terminal frontend.
  `--print`, `--rpc`, plain/headless runs, and Serve never report — the
  equivalent of the `ctx.mode !== "tui"` gate the Pi extension applies.
- **Released on exit.** When octet leaves the terminal it releases the same
  source's lifecycle authority, so a late report cannot reclaim a pane whose
  agent has exited. Herdr's Pi integration relies on process-exit detection
  instead; explicit release is the practice Herdr documents for custom agents.

## Transports

| Platform | Transport |
| --- | --- |
| Linux, macOS | Direct socket IPC on `HERDR_SOCKET_PATH` (canonical newline-delimited JSON) |
| Windows | The documented CLI wrapper at `HERDR_BIN_PATH`, one argv list per report with a bounded wait |

Reporting requires `HERDR_ENV=1`, `HERDR_PANE_ID`, and one of the two transports,
all of which Herdr exports into every managed pane. Anything missing means no
reporting at all. The restore plugin declares `linux` and `macos` only: the
reporting side already works on Windows, but the pane-resume pass has not been
verified there.

## Session restore

Herdr's native session restore resumes only agents whose kind it knows how to
launch (`pi --session`, `codex resume`, `claude --resume`, and so on). octet is
not one of those kinds, and Herdr states that adding a new agent needs a Herdr
binary update rather than an agent-side integration.

So octet ships the resume path itself, through Herdr's documented plugin
surface:

```sh
octet herdr install-plugin
```

That writes one minimal manifest to `~/.octet/herdr-plugin/herdr-plugin.toml` and
links it with `herdr plugin link`. The plugin declares a single `[[startup]]` hook
— `octet herdr restore`, run with the absolute path of the octet that installed
it — and nothing else: no events, no actions, no panes, no build steps, and no
state outside octet's own directories. Remove it again with
`octet herdr uninstall-plugin`, and inspect it with `octet herdr status`.

Herdr runs startup hooks once for each enabled plugin after it restores the
session and its API socket is ready, which is the only moment at which a
restored pane can be handed back to octet. The pass then:

- reads octet's owner-private pane records (`~/.octet/herdr/panes/`), including
  the session-store root and workspace needed to resolve each opaque id;
- keeps only records written by **this** Herdr session, still inside the 14-day
  retention window, whose pane still exists;
- skips a pane that already hosts an agent, or whose current directory is not
  the recorded one, so a session id can never resolve against the wrong
  workspace;
- resumes at most 16 panes with `octet --resume <id> --session-dir <root> --workspace <workspace>`.
  Session ids are token-validated and absolute paths
  are bounded and shell-quoted; unsafe values are skipped with a reason. CLI
  pane-list output is drained while Herdr runs and capped at 512 KiB.

While octet runs, the version-2 record is refreshed at startup and whenever the
active session changes, atomically under an owner-only directory and file. At
most 64 records are admitted. Older record versions are ignored; a running
session writes the new format at its next startup or session change. The record
is deleted on a **deliberate** exit, so a later restore does
not resurrect a session you closed, and kept when the terminal itself went away:
Herdr delivers `SIGHUP` to a pane's foreground process on a server stop
(measured against Herdr 0.9.0), which is exactly the workspace-teardown case.

A pane that cannot be resumed comes back as a shell in the same directory. For
manual recovery use `octet --resume <id>` with the original `--session-dir` and
`--workspace` settings if they were customized.

## Verify it yourself

With octet running in a Herdr pane:

```sh
herdr agent list                 # octet, with its live state
herdr pane get "$HERDR_PANE_ID"  # agent + agent_status for this pane
```

Watch the state while you work: it is `working` from the moment a prompt is
accepted, `blocked` while an approval prompt is on screen, and `idle` once the
run settles. Quitting octet clears the agent from `herdr agent list`.

To verify restore, install the plugin, run octet in a pane, then stop and start
the Herdr session (`herdr session stop <name>`, then `herdr --session <name>`):
the pane comes back and octet resumes its conversation in it. `octet herdr
status` lists the recorded panes.

## Implementation

`crates/octet-coding-agent/src/herdr.rs` owns the reporter: transport selection,
the lifecycle mapper, sequence handling, bounded encoding, and the release path.
`crates/octet-coding-agent/src/herdr/restore.rs` owns the pane records and the
restore pass; `crates/octet-coding-agent/src/herdr/plugin.rs` owns the generated
plugin manifest. The frontend supplies three events
(`crates/octet-coding-agent/src/modes/interactive.rs`): ready at startup, prompt
accepted, and session changed. Everything else is derived from the run stream in
`crates/octet-coding-agent/src/tui/view.rs`, so the reporter cannot disagree with
what octet is actually doing.

## Related

- [tmux setup](tmux.md) — modifier-key configuration for tmux, a different
  concern from agent state reporting.
- [Subagents](subagents.md) — opening workers in panes is a separate,
  ownership-gated feature; it does not depend on this integration.
