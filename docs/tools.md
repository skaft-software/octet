# Tools and permissions

[Documentation](README.md) · [Security](../SECURITY.md) · [CLI](cli.md#tools-and-limits)

Choose a narrow tool surface for a review:

```sh
octet --safe-mode --tools read,search --no-context-files --offline
```

This enables read/search only, skips context files and optional discovery, and
keeps approval policy controlled. It is not a network sandbox: inference still
contacts the selected provider. Use OS isolation for untrusted work.

## Built-in tools

| Tool | Purpose | Default registration |
| --- | --- | --- |
| `read` | Bounded text reads with line-oriented output; [supported media](media.md). | On |
| `edit` | Exact, stale-aware replacements under the workspace policy. | On |
| `write` | Create or replace complete files under the workspace policy. | On |
| `bash` | Bash-compatible commands with bounded output, timeout, cancellation, and process-group cleanup. | On |
| `search` | Ripgrep-backed workspace search. | Opt-in |

The final allowlist creates both the model-visible schemas and executable
registry: disabled tools cannot remain advertised. Registration is not approval
for an effect. Only explicitly parallel-safe pure/workspace-read calls overlap;
shell and mutation effects stay serialized, even when a model batches tool calls.

| Restriction | Launch option |
| --- | --- |
| Explicit allowlist / exclusions | `--tools read,search` / `--exclude-tools bash` |
| No file mutation | `--no-edit` disables both edit and write. |
| No complete-file writes | `--no-write` |
| No commands | `--no-process` or equivalent `--no-shell` |
| No tools | `--no-tools` |

## Authority profiles

**Full access is the default.** `unsafe_host` (`UnsafeHost`) admits
authoritatively classified effects with the octet process's ambient OS authority,
subject to tool and sandbox gates. It is appropriate only within a separately
isolated account, container, VM, or platform sandbox—not as containment itself.
Unknown effects always fail closed.

Select `effect_policy`, `OCTET_EFFECT_POLICY`, or `--effect-policy`:

| Value | Effect admission |
| --- | --- |
| `unsafe_host` | Default full access for classified effects. |
| `controlled` | Pure/workspace reads; confirmation for workspace mutation and non-whitelisted bash calls. Conservative known-safe read-only bash calls may be auto-approved; other ambient effects are denied. |
| `controlled_bash_approval` | Workspace-mutation approval and one-shot approval for **every** bash process call; other ambient effects denied. |

Full access implicitly trusts selected executable extensions, but they remain
**disabled by default** until explicitly enabled. Trust does not bypass process
gates, source validation, bundle integrity, or protocol checks, and implicit
trust is never persisted as a grant.

`--safe-mode` selects `ControlledBashApproval`, conflicts with `--effect-policy`,
and forces `allow_external_paths = false`. It removes implicit extension trust.
Executable extensions are discovered but never started in safe mode, even with
explicit trust and process/shell gates enabled: startup still requires
`unsafe_host`. This does not add an OS sandbox or change the one-shot approval
required for every bash call. A trusted project may tighten but not relax the
global authority profile. Approval cannot
undo an already admitted action. See the [effect contract](design/octet-agent.md#effect-admission-boundary).

`--safe` is a hidden compatibility alias. `--yolo` and its configuration and
environment forms are no longer accepted.

Full-access CLI launches default to `allow_external_paths = true`. Set it to
`false` for workspace-local built-in file access; `--safe-mode` forces false.
File-path restrictions do not contain shell commands or extension processes.

## Shell selection

In full-access mode, bash has the current user's authority. Every complete
command is passed to one selected shell with `-c`. Unix selection is explicit
`shell_path`, then `/bin/bash`, `bash` on `PATH`, then `sh`; `$SHELL` is not read.
`--shell-path PATH` selects a shell; `--allow-shell` does not bypass an independent
process or effect gate. Policy diagnostics reveal only `configured`,
`system_bash`, `path_bash`, or `sh_fallback`, never a path or digest.

The documented configuration example uses `bash_timeout_secs = 120` and
`max_output_bytes = 1048576`; [configuration](configuration.md#settings) records
these alongside capability controls. Capture limits differ from the TUI's
collapsible preview: expanding a panel cannot restore discarded bytes.

## Recovery and security

- Descriptor-relative no-follow file operations prevent parent-symlink replacement from redirecting built-in reads or mutations. Shell commands and extension processes are not contained by a file-path guard.
- Provider streams, discovery, config, credentials, context, sessions, local reads, and tool inputs/results have byte/count bounds.
- Complete session records survive; torn final appends are narrowly repairable. Unresolved mutating calls are indeterminate and never silently replayed.
- Cancellation covers provider streams, retry waits, compaction, tools, delegated agents, and descendant process/agent groups.
- Delegation directories/files are owner-private and descriptor-bound. Spawns, status, and interrupts sync before visibility; journal failure cancels the team and rejects new work. [Delegation provenance](design/octet-agent.md#v2-task-delegation).
- A positively classified pre-send connection failure (including a connect timeout) is different from an ambiguous accepted request. Sending a POST, awaiting headers, or losing its body can leave execution indeterminate. No visible text does **not** make replay safe. Unqualified requests retain the conservative no-body-replay default. Only host-qualified Codex local-function inference may be replaced before assistant commit, with separate finite streamed-inference and HTTP-admission retry budgets and unknown usage guarded under hard cumulative cost/token limits. HTTP 5xx, including gateway 504, is not evidence of zero usage; ambiguous status failures require durable uncertainty before replacement. Unknown exposure is durably recorded separately from known usage and survives success, resume, and checkout; numeric usage/cost then represents known subtotals, not complete totals. Completed local tool effects/results are not replayed. Provisional TUI removal is not replay permission or proof of remote cancellation. See [candidate recovery qualification](qualification/v0.7.4-recovery.md); deterministic regressions do not qualify live recovery, Codex parity, or weeks-scale endurance.
- Credential files are owner-private; headers, debug output, provider diagnostics, and bounded session export redact secrets. Redirects are disabled and terminal controls are neutralized.

Release checks include protocol/adversarial-stream fixtures, filesystem races,
VT100/PTY shutdown tests, workspace tests, `cargo audit`, and `cargo deny`
advisory/license/ban/duplicate/source policy gates. These are required checks,
not results from this documentation draft. See [contributing](../CONTRIBUTING.md#tests)
and the [security policy](../SECURITY.md).
