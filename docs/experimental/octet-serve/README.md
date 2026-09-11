# Experimental `octet serve`

This guide describes the experimental Serve source in octet **0.7.5**. With
[the matching octet version installed](../../installation.md) and its Serve
assets available on the
[exact GitHub release](https://github.com/skaft-software/octet/releases/tag/v0.7.5),
install the package and start the graphical client:

```console
octet extension install octet-serve
octet serve --port 0
```

For development from a source checkout, use
`cargo run --features serve -- serve --port 0`.

This starts a headless host for the launch workspace and opens its local web
client. `--port 0` requests an available port. Add `--no-open` to skip opening the
browser; `--web-root <directory>` selects a development asset directory.

The [0.7.5 notes](../../releases/v0.7.5.md) describe source changes; the exact
GitHub release is authoritative for availability, signed assets, and
public-install verification. [0.7.4](../../releases/v0.7.4.md) is a published,
immutable release with signed Serve packages and installation checks; those
results do not qualify 0.7.5. Serve remains experimental. Live-provider and
native-host audio checks are optional and **NOT RUN** in this source review.
Package smoke does not qualify private-LAN access, actual-terminal/SSH behavior,
endurance, or every graphical media, recovery, and visual journey.

<a id="product-contract"></a>

## Use tasks

- Open the app root for a fresh provisional task, an explicit task route to
  restore that task, or `/overview` to browse task inventory without creating or
  opening a task. The overview does not clear an already selected task.
- Previous, pinned, and running tasks stay in the sidebar. Tasks are independent
  sessions and can run concurrently; observers of one task share one host owner.
- Send a prompt, stop a run, steer it, or queue a follow-up. Model and reasoning
  choices come from the host. Edit, retry, fork, and branch checkout require an
  idle, committed boundary.
- Use `@` to select trusted project files as explicit context. Use `/` for
  host-admitted commands, prompt templates, skills, and enabled extensions.
  Commands requiring interactive extension confirmation are unavailable.
- Attach PNG, JPEG, GIF, or WebP images, or bounded text, Markdown, and ordinary
  PDFs. Model support still governs image input. The production Serve host does
  not accept audio.

The transcript is the main task view. The command center sorts and searches
host-owned task state; sources, changes, outputs, approvals, and progress appear
only when structured evidence supports them. There is no extra agent mode or
synchronized TUI.

## Access and authority

The host binds **IPv4 loopback only**. A one-use launch capability is exchanged
for an ephemeral **HttpOnly, SameSite=Strict** browser cookie before API or
event-stream access. Host, Origin, and Fetch Metadata checks restrict requests
to the local application. Keep the launch capability private.

Browser authentication is not project trust or an agent sandbox. Authority
choices come from host configuration, but selecting a narrower label does not
reconfigure tool or process enforcement. Set restrictions on the host before
launch; do not rely on the picker for isolation. Enabled commands run with the
local user's OS authority; use a restricted user, container, VM, or OS sandbox
for hostile work. Pairing would not grant project trust, Remote Read, or more
tool authority.

**LAN pairing is not implemented.** There is no working `--lan`, `--demo`, or
`--local-only` switch. Do not expose this listener through `0.0.0.0`, a proxy, or
port forwarding. The [LAN specification](lan-pairing.md) describes a separate,
opt-in pinned-TLS transport with explicit pairing and revocable device
credentials; it is not setup guidance. [Native apps](native-delivery.md) are
also unimplemented.

## Terminal and recovery

The terminal appears only when host configuration allows process execution. It
starts a local shell in the configured workspace and retains at most four terminals.
Browser disconnect or detach retains the shell; host shutdown stops retained
shells. Closing an inspector or preview is only a presentation action, not a
stop command. Descendant cleanup is bounded, not OS-level process containment.

Reconnect replays missing events or replaces state with an authoritative
snapshot on a replay gap. Repeated command IDs do not execute twice. Browser
text and attachment drafts are session-scoped and clear after acknowledged
submission, but accepted queued follow-ups do **not** survive a host restart.
Conversation checkout and forks do not undo filesystem or other external effects.

Archive and trash retain tasks for later access or restore. Permanent deletion
requires the exact confirmation phrase and uses a crash-recovery journal;
missing required stores fail before commit. Shared payloads and
conversation-content-free inference accounting are retained. See the complete
[deletion and recovery contract](../../design/serve-lifecycle-safety.md#permanent-session-deletion).
These are source-described contracts, not a completed current-version recovery
qualification.

<a id="build-install-and-release-gates"></a>

## Install or update a package

With octet `0.7.5`, first check that the matching Serve assets are available on
the [exact GitHub release](https://github.com/skaft-software/octet/releases/tag/v0.7.5).
Then install or update the public package:

```console
octet extension install octet-serve
octet extension update octet-serve
```

For a reviewed matching local archive instead:

```console
octet extension install --path ./octet-serve-0.7.5-TARGET.tar.gz
octet extension list
octet serve
```

Local archive installation does not need GitHub network access. The package
requires exactly `=0.7.5`. Replace `TARGET` with `x86_64-unknown-linux-gnu`,
`x86_64-apple-darwin`, or `aarch64-apple-darwin`; Linux musl is unsupported.
A local build/archive is not evidence of signed publication.

Update reinstalls the package matching the running octet version; it is not an
independent upgrade to a different runtime version.
`octet extension remove octet-serve` removes package files, not Serve sessions or
other user data. See [package and release details](../../../extensions/octet-serve/README.md#package-and-release-reference).

<a id="explicit-exclusions"></a>

## Availability limits

Production live previews and child-agent trees are disabled. Durable source,
diff, and output evidence covers successful built-in `read`,
`read_skill_resource`, `edit`, and `write`, not all Bash or extension mutations.
There is no arbitrary-folder import from the browser, MCP or LSP management,
extension catalog/lifecycle UI, scheduling, WAN access, multi-host replication,
or hosted account service. Missing capabilities should stay hidden, not appear
as empty dashboard sections.

<a id="package-boundary"></a>
<a id="first-web-cut"></a>

## Reference

- [Implementation coverage and limitations](current-state.md) — maintainer reference.
- [Architecture](architecture.md) and [lifecycle safety](../../design/serve-lifecycle-safety.md) — technical contracts.
- [Web acceptance](web-acceptance.md) and [provider acceptance](provider-acceptance.md) — criteria, not a current pass.
- [Historical checklist](p0-p1-delivery.md) and [validation record](current-state.md#validation-evidence) — evidence with its original scope.
- [Project](https://github.com/orgs/skaft-software/projects/5) — work tracking.
