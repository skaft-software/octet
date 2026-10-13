# Security

## Report a vulnerability

Use [GitHub private vulnerability
reporting](https://github.com/skaft-software/octet/security/advisories/new), not
a public issue. Include the affected version or commit, platform, impact and
steps to reproduce. If the form isn't available, contact the repository owners
privately through the GitHub organization.

## Supported versions

octet is pre-1.0 software. Report issues against a published release or the
current source; include the version and commit, because behavior may change.
Candidate builds are not published releases. See the versioned release record
for availability and qualification status.

## Permissions

**octet runs with your operating-system permissions. It is not a sandbox.**

- Full access is the default. Enabled shell commands and extensions can reach
  files, the network and other processes with your permissions.
- `--safe-mode` asks before file changes and every shell call. It doesn't start
  executable extensions **unless you've granted host authority** for the
  selected source (or explicitly selected `--extension-dir`). Granted extension
  code runs as a host process with your OS permissions, outside the tool-effect
  broker, so safe mode is **not** a sandbox. Approved actions still affect the
  real workspace.
- Full-access CLI launches default to `allow_external_paths = true`, which lets
  the built-in file tools reach outside the workspace. Set it to `false` for
  workspace-local file access. `--safe-mode` forces `false`. File-path limits
  don't contain shell commands or extension processes.
- Project configuration, `AGENTS.md` and project skills load only with
  `--workspace-trusted`. Trusted project settings can't weaken global safety
  limits.
- `--no-edit` disables both editing and writing. `--tools read` allows only the
  read tool. `--no-tools` disables all tools.

For untrusted repositories, use a disposable container, VM or restricted
account. Expose only the files it needs, keep personal credentials out, and
restrict network access at the OS level.

## Data and privacy

Files and media included in a prompt may be sent to the selected model provider.
`--offline` disables optional discovery, **not inference network access**.
Sessions can contain private material, so check exports before sharing them.

Credentials and sessions use private files with bounded parsing. Session writes
are locked and checked for concurrent changes. Interrupted mutating tool calls
aren't replayed automatically. Cancelling stops provider work, tools and
compaction, but it can't undo an action that already happened.

On first use, octet may copy Codex CLI credentials into its private store. It
doesn't modify the Codex source. The native host denies headless approval
requests, and cancelling it means terminating its dedicated process group.

## What to report

Report permission or project-trust bypasses, unintended disclosure, secret
logging, session corruption, duplicate mutations after recovery,
terminal-control injection and exploitable dependency vulnerabilities.

Model mistakes and prompt injection are known risks. A model using them to
bypass a configured octet security boundary is still a vulnerability.
