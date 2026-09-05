# octet security policy

## Supported versions

octet is pre-1.0 software. This checkout declares the unpublished 0.7.0 source
version; it is not yet a newly supported or qualified release. The inherited
released product is Ygg, whose latest `0.6.x` release remains the historical
security-fix target. See the [0.7.0 qualification gates](docs/releases/v0.7.0.md).

The current contracts use `octet`, `OCTET_*`, and `.octet`. The clean break
provides no Ygg aliases, old-root fallbacks, or automatic earlier-first-party
Hamr/Ygg imports. Unrelated installed binaries and data remain untouched.
Third-party Codex interoperability and explicitly invoked Pi import/restore
are separate, supported boundaries; neither permits source-store mutation.

## Boundary and defaults

octet runs as the current operating-system user. It is **not** an OS sandbox:
any effect admitted through the explicit unsafe-host policy inherits the
process's filesystem, environment, subprocess, and network authority. Use an
isolated account, container, VM, or platform sandbox when a repository or model
endpoint is not trusted.

octet nevertheless treats its own policy and persistence boundaries as security invariants:

- Explicit built-in file paths are workspace-only by default. Unix file reads and mutations use descriptor-relative, no-follow operations so validation cannot be redirected by a parent-symlink replacement.
- Project `.octet/config.toml`, workspace `AGENTS.md`, and workspace skills are ignored unless the user passes `--workspace-trusted`.
- Trusted project settings may tighten global authority/resource floors but cannot relax them. Environment and explicit CLI settings remain user-controlled higher-trust layers.
- Every model-requested tool call passes through a host-owned effect broker. By default, octet uses `UnsafeHost`, allowing authoritatively classified effects to use ambient authority after the remaining gates pass. `--safe-mode` selects `ControlledBashApproval`, requiring interactive approval for workspace mutation and every `bash` process call while still denying other ambient host/process/network/delegation/extension effects. Workspace-mutation grants bind the exact principal, run, catalog generation, provider call ID, tool, classification, arguments, and policy version; they are short-lived, atomically single-use, reserved before hooks, and consumed immediately before execution.
- Context/config/credential files must be bounded regular files. Workspace context symlinks and special files are rejected.
- Disabled tools are removed from both the provider schema and execution registry. `--no-edit` disables `edit` and `write`; `--tools read,search` and `--no-tools` provide complete allowlisting.
- Arbitrary process execution and shell execution are treated as equivalent authority. `bash` requires both compatibility gates, and in `Controlled` mode only safe read-only commands are auto-approved by default while all others need explicit approval.
- Crash replay requires both a tool's static replay-safe declaration and an exact `Pure` or `WorkspaceRead` host classification. Every other unresolved call is paired with an indeterminate result and is not executed.
- Session mutation uses advisory interprocess locking, stale-generation checks, private permissions, bounded parsing, and synced records. Session listing is byte-for-byte read-only.
- The native `octet-host` keeps stdout protocol-only and bounds inbound and outbound NDJSON frames. It confines resumed sessions to regular files in the selected session directory, validates inline-provider and image inputs, denies headless tool confirmations, and cancels typed input requests. Protocol v1 has no in-band abort, so consumers must launch each host in a dedicated process group and terminate that group to cancel it.
- On first use, octet may copy bounded valid third-party Codex CLI credentials from `~/.codex/auth.json` into `~/.octet/credentials/codex.json`, with owner-only permissions and a cross-process lock. It never reads earlier-first-party Hamr/Ygg stores, modifies or deletes the Codex source, or includes credential values in import diagnostics.
- Serve project browsing and editing resolves opaque project IDs to a revalidated root identity, then uses descriptor-relative no-follow traversal. Atomic writes recheck target identity and sync both content and the owning directory.
- Serve owns Git and PTY process groups through bounded graceful/forced cleanup, including descendants that retain output descriptors. This prevents ordinary timeout/shutdown leaks; it does not restrict what an enabled command may access.
- Serve permanent deletion journals intent before removing the transcript, retries idempotent sidecar cleanup after interruption, retains payloads referenced by another session, and fails before commit when a required store is unavailable. Conversation-content-free append-only inference accounting remains host-level history.
- Provider streams, discovery responses, context, configuration, credentials, sessions, tool arguments/results, and local file reads have hard aggregate limits.
- Serve accepts only bounded, classic single-revision PDFs for partial text extraction. An iterative raw-syntax preflight rejects excessive direct nesting before the audited `lopdf 0.42.0` parser runs; parser version, object/page/decompression limits, and a deeply nested regression are release gates.
- Run cancellation reaches provider streaming, retry waits, tools, and autonomous compaction. Once cancellation wins a request race, no summary or usage record from that request is committed.

These controls reduce accidental authority and defend documented octet boundaries.
`--safe-mode` is an admission policy, not full malicious-worker containment:
permitted workspace reads can enter provider-visible context, approved mutations
affect the live workspace, and octet does not yet provide overlay promotion,
information-flow labels, a native-code isolation backend, or a dedicated
egress/secret broker. In safe mode, every host process call requires explicit
confirmation and file effects remain workspace constrained. These controls do not
contain an effect admitted by the default full-access mode; in particular, a
`bash` call can read credentials, access the network, and start descendants with
the user's authority if no additional policy prevents it.

## Recommended untrusted-repository workflow

Use OS isolation and expose only the repository copy that may be changed. At minimum:

1. Start a disposable container/VM or restricted user account with no personal credentials.
2. Mount only a disposable workspace; do not mount SSH, cloud, browser, package-registry, or provider credential directories.
3. Restrict outbound network to the selected model endpoint, or use a local endpoint.
4. Run without project resources and without commands initially:

   ```sh
   octet --offline --no-context-files --tools read --workspace /workspace
   ```

5. Inspect project instructions/config before choosing `--workspace-trusted` or enabling mutation/command tools.

Controlled forces `allow_external_paths=false` for built-in file operations.
That path gate does not constrain paths opened by a child process admitted under
UnsafeHost; only the OS isolation boundary can do that.

## In scope

Please report, among other issues:

- bypass of workspace path, project trust, tool allowlist, cancellation, stream/resource, credential, or session-integrity guarantees;
- unauthorized disclosure caused by octet loading or transmitting a local resource;
- session corruption or silent duplicate mutating work;
- secret exposure in logs/errors;
- terminal control-sequence injection in terminal-safe modes;
- remotely reachable dependency vulnerabilities with demonstrated impact;
- privilege-boundary crossings or unauthorized remote interfaces.

Prompt injection and model mistakes remain expected risks, but a model using them to bypass a configured octet boundary is in scope.

## Private reporting

Do not open a public issue for a suspected vulnerability. Use GitHub private vulnerability reporting:

**https://github.com/skaft-software/ygg/security/advisories/new**

Include impact, reproduction steps or a proof of concept, affected version/commit, platform, and known mitigations. If that private form is unavailable, contact the repository owners privately through the GitHub organization before disclosing details.
