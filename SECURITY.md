# Security

## Report a vulnerability

Please use [GitHub private vulnerability reporting](https://github.com/skaft-software/ygg/security/advisories/new),
not a public issue. Include the affected version or commit, platform, impact,
and steps to reproduce. If the form is unavailable, contact the repository
owners privately through the GitHub organization.

## Supported versions

octet is pre-1.0 software. Version 0.7.0 is the current release. Reports
against the release or current source are welcome; include the version and
commit because behavior may change.

## Permissions

**octet runs with your operating-system permissions. It is not a sandbox.**

- Full access is the default. Enabled shell commands and extensions can access
  files, the network, and other processes with your permissions.
- `--safe-mode` asks before file changes and every shell call. It does not start
  executable extensions. Approved actions still affect the real workspace.
- Full-access CLI launches default to `allow_external_paths = true`, allowing
  built-in file tools outside the workspace. Set it to `false` for workspace-local
  file access; `--safe-mode` forces false. File-path restrictions do not contain
  shell commands or extension processes.
- Project configuration, `AGENTS.md`, and project skills load only with
  `--workspace-trusted`. Trusted project settings cannot weaken global safety
  limits.
- `--no-edit` disables both editing and writing. `--tools read` allows only the
  read tool; `--no-tools` disables all tools.

Use a disposable container, VM, or restricted account for untrusted repositories.
Expose only the files it needs, keep personal credentials out, and restrict
network access at the OS level.

## Data and privacy

Files and media included in a prompt may be sent to the selected model provider.
`--offline` disables optional discovery, **not inference network access**.
Sessions can contain private material; inspect exports before sharing them.

Credentials and sessions use private files with bounded parsing. Session writes
are locked and checked for concurrent changes. Interrupted mutating tool calls
are not automatically replayed. Cancellation stops provider work, tools, and
compaction, though it cannot undo an action that already happened.

On first use, octet may copy Codex CLI credentials into its private store. It
does not modify the Codex source. The native host denies headless approval
requests; cancelling it requires terminating its dedicated process group.

## What to report

Report permission or project-trust bypasses, unintended disclosure, secret
logging, session corruption, duplicate mutations after recovery, terminal-control
injection, and exploitable dependency vulnerabilities.

Model mistakes and prompt injection are known risks. A model using them to
bypass a configured octet security boundary is still a vulnerability.
