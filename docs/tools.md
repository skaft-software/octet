# Tools and permissions

[Documentation](README.md) · [Security](../SECURITY.md) · [CLI](cli.md#tools-and-limits)

The narrowest setup, for reviewing code:

```sh
octet --safe-mode --tools read --no-context-files --offline
```

This allows read only, skips context files and optional discovery, and keeps
approvals on. It isn't a network sandbox: inference still contacts your
provider. Use OS isolation for untrusted work. Content search needs the default
`unsafe_host` effect policy (`octet --tools read,search --no-context-files
--offline`), because `search` runs ripgrep as a native child and the controlled
policies that `--safe-mode` selects deny a non-`bash` host process.

## Built-in tools

| Tool | What it does | Default |
| --- | --- | --- |
| `read` | Reads text files line by line, within a size limit. Also handles [supported media](media.md). | On |
| `edit` | Exact replacements that detect stale file content. | On |
| `write` | Creates or replaces whole files. | On |
| `bash` | Runs Bash-compatible commands with output limits, a timeout, cancellation and process-group cleanup. | On |
| `search` | Ripgrep search of the workspace. | On |

A tool you turn off is never advertised to the model. Having a tool isn't
permission to use it. Only parallel-safe reads run together. Shell and file
changes run one at a time, even if the model batches calls. `search` runs
ripgrep as a native child, so `--no-process`/`--no-shell` remove it from the
surface as well, and controlled effect policies deny its process calls.

| To do this | Use |
| --- | --- |
| Allow only some tools, or exclude some | `--tools read,search` or `--exclude-tools bash` |
| Block file changes | `--no-edit` (turns off `edit` and `write`) |
| Block whole-file writes | `--no-write` |
| Block commands | `--no-process` or `--no-shell` |
| Turn off every tool | `--no-tools` |

## Optional JavaScript composition

[octet-codemode](../extensions/octet-codemode/README.md) is a first-party octet
bundle that vendors Pi's MIT-licensed code mode runtime and batches and chains
the same enabled tools in that offline QuickJS/WASM guest. Explicit enablement
and the trusted Node launcher are required; this does not weaken safe mode,
effect policy, tool exclusions or approvals. `on` retains direct tools; `only`
advertises composition tools while ordinary tools stay nested-only.

Nested core `read` returns unnumbered bounded `content` with path/hash/line and
continuation metadata. `search` returns ordered `matches` with path, line, text,
context/clipping flags, `total` and `truncated`. `bash` returns independently
bounded raw `stdout`/`stderr` (up to 1 MiB source bytes each, additionally bounded
by JSON encoding), exit status, byte counts and completeness/truncation flags.
Direct tool text stays unchanged. Schema-less tools resolve to text, not
implicitly parsed JSON, and nested media/raw streams do not automatically enter
chat. Only explicitly returned output or `text()`/`image()` is published.

The host limits a parent to 256 calls and 30 seconds, with at most four safe
observations in parallel; mutations remain exclusive. Completed effects survive
script failure; successful store writes are private branch-scoped metadata.
Nested model usage cannot bypass session ceilings or be priced at the chat
model's rate. Tools without host-authoritative usage bounds are refused under
hard ceilings. See [extension composition](extensions.md#tool-composition).

<a id="authority-profiles"></a>

## Permissions

**Full access is the default.** octet runs with your account's full authority
(`unsafe_host`, or `UnsafeHost`). That's only appropriate inside an account,
container, VM or platform sandbox you've isolated separately. Effects octet
can't classify are always refused.

Pick a policy with `effect_policy`, `OCTET_EFFECT_POLICY` or `--effect-policy`:

| Value | What happens |
| --- | --- |
| `unsafe_host` | The default. Full access for effects octet can classify. |
| `controlled` | Workspace reads and pure calls run freely. File changes and bash calls not on the safe list ask first. Known-safe read-only bash calls may be auto-approved. Other effects are denied. |
| `controlled_bash_approval` | File changes ask first, and **every** bash call asks first, once. Other effects are denied. |

Full access implicitly grants host authority to the executable extensions you
select, but they stay **disabled by default** until you enable them. A grant
doesn't bypass `--no-process` or `--no-shell`, source validation, bundle
integrity or protocol checks, and implicit authority is never written to your
user config.

`--safe-mode` picks `controlled_bash_approval` (`ControlledBashApproval`) and
forces `allow_external_paths = false`. It can't be combined with
`--effect-policy`. It also removes implicit host authority: an enabled extension
starts only with a persistent per-source grant, an explicit one-run grant, or a
selected `--extension-dir`. The broker still governs tool effects, but
**extension code runs outside the broker with your OS permissions**. Safe mode
isn't an OS sandbox, and approving a tool call doesn't contain the extension
process. Every bash call still needs one-shot approval. A trusted project can
tighten the global policy, never loosen it. Approving can't undo an action that
already ran. [Effect contract](design/octet-agent.md#effect-admission-boundary).

Full-access launches default to `allow_external_paths = true`. Set it to `false`
to keep the built-in file tools inside the workspace. That doesn't contain shell
commands or extension processes. `--safe` is a hidden alias for `--safe-mode`,
and `--yolo` and its settings and environment forms are no longer accepted.

<a id="shell-selection"></a>

## Choose a shell

In full-access mode, bash runs with your user's authority. Each complete command
goes to one shell with `-c`. On Unix, octet uses the first of: `shell_path`,
`/bin/bash`, `bash` on `PATH`, then `sh`. It doesn't read `$SHELL`. Use
`--shell-path PATH` to choose one. `--allow-shell` doesn't bypass a separate
process or effect gate. Diagnostics report only `configured`, `system_bash`,
`path_bash` or `sh_fallback`, never a path.

The example [settings](configuration.md#settings) use `bash_timeout_secs = 120`
and `max_output_bytes = 1048576`. Expanding a panel in the TUI can't restore
output that was discarded at capture.

<a id="bash-output-and-temporary-spills"></a>

## Bash output and temporary files

Bash drains stdout and stderr with bounded head and tail previews. A truncated
result may include `full_output_path` when the whole stream was kept, or
`partial_output_path` when only a prefix could be saved. Neither a partial path
nor a missing path promises that full output is recoverable. Saved output is
private and temporary, so a path in an old result may have expired.

<details>
<summary>Spill limits and flags</summary>

- Spill files hold at most **16 MiB per stream** and share a **64 MiB / 32-file
  budget per tool instance** (the resource owner), counting active captures.
- Under pressure the oldest files expire first. Active captures aren't evicted.
  If they use up the allowance, further spill bytes are discarded while the
  pipes keep draining.
- `spill_truncated=true` means storage limits were hit (the result has
  `partial_output_path`, never `full_output_path`). `spill_error=true` means
  capture or storage failed. `spill_expired=true` means a file was evicted
  during capture, and no path is advertised.
- Dropping the tool removes its files, and paths clean up when the last agent
  with that owner closes. They aren't durable session artifacts.
- Disk writes and cleanup run in bounded workers, off the async input and run
  path.

Rust embedders build the stateful tool with `Default`, not a unit value:

```rust
use octet_agent::{BashTool, ExtensionHost};

let mut host = ExtensionHost::new();
host.tool(BashTool::default()); // The host retains the tool and its spill store.
```

Keep the owning tool alive for the frontend's lifetime instead of building one
per call. The RPC frontend keeps an `Arc<BashTool>` across commands, so its
spill limits are shared across them, not reset per result.

</details>

## Recovery and security

octet never silently replays a mutating call that has no recorded result, and it
doesn't resend a request that may have been accepted, even if no text was shown.
Credentials, headers, debug output, provider errors and exports are redacted.
The policy is in [Security](../SECURITY.md), and the guarantees are in the
[agent design](design/octet-agent.md#commit-and-cancellation-invariants).

<details>
<summary>Provider retries and unknown usage</summary>

A pre-send connection failure that's positively classified (including a connect
timeout) is different from an ambiguous accepted request. Sending a POST,
waiting for headers or losing the body can leave execution indeterminate.
Showing no text doesn't make a replay safe, and requests that aren't qualified
keep the conservative no-body-replay default.

Only host-qualified Codex local-function inference may be replaced before the
assistant message is committed, with separate finite retry budgets for streamed
inference and HTTP admission, and with unknown usage guarded by hard cumulative
cost and token limits. An HTTP 5xx, including a gateway 504, isn't evidence of
zero usage, and an ambiguous status failure needs durable uncertainty recorded
before a replacement. Unknown exposure is recorded separately from known usage
and survives success, resume and checkout, so the numeric usage and cost are
known subtotals, not complete totals. Completed local tool effects and results
aren't replayed. Removing a provisional line from the TUI isn't permission to
replay and isn't proof of remote cancellation. See the [agent recovery
contract](design/octet-agent.md#in-process-provider-recovery). Deterministic
tests don't qualify live recovery or weeks-long endurance.

</details>

<details>
<summary>More safeguards</summary>

- File tools use no-follow, descriptor-relative operations, so a swapped symlink
  can't redirect them. Shell commands and extension processes aren't covered.
- Provider streams, config, credentials, context, sessions, reads and tool
  results all have size and count limits.
- Complete session records survive, and a torn final append can be repaired
  narrowly.
- Cancel reaches provider streams, retries, compaction, tools, subagents and
  their child processes.
- Delegation directories and files are owner-private and descriptor-bound.
  Spawns, status and interrupts sync before they're visible, and a journal
  failure cancels the team and rejects new work. [Delegation
  provenance](design/octet-agent.md#v2-task-delegation).
- Credential files are owner-private. Redirects are off, and terminal control
  characters are neutralized.
- How this is tested: [Contributing](../CONTRIBUTING.md#tests).

</details>
