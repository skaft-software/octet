# Experimental `octet serve`

This guide describes experimental Serve in octet **0.8.0**. Install the
[version-matched package](#install-or-update-a-package), or use a reviewed
source checkout:

```console
cargo run --features serve -- serve --port 0
```

A reviewed local package must match the source-built host exactly (see [package
installation](#install-or-update-a-package)). Catalog installation needs
verified matching published assets from the version-pinned release.

This starts a headless host for the launch workspace and opens its local web
client. `--port 0` picks an available port. `--no-open` skips opening the
browser, and `--web-root <directory>` selects a development asset directory.

The [0.8.0 release notes](../../releases/v0.8.0.md) describe the changes and
link the current signed-asset and public-install evidence. The historical
[0.7.6](../../releases/v0.7.6.md) and [0.7.4](../../releases/v0.7.4.md) records
keep their own evidence, and those results don't qualify 0.8.0. Serve is still
experimental. Live-provider and native-host audio checks are optional and **NOT
RUN** in this source review. Package smoke tests don't qualify private-LAN
access, real terminal or SSH behavior, endurance, or every graphical media,
recovery and visual journey.

<a id="product-contract"></a>

## Use tasks

- Open the app root for a fresh provisional task, an explicit task route to
  restore that task, or `/overview` to browse the task list without creating or
  opening one. The overview doesn't clear a task you already selected.
- Previous, pinned and running tasks stay in the sidebar. Tasks are independent
  sessions and can run at the same time. Everyone watching a task shares one
  host owner.
- Send a prompt, stop a run, steer it or queue a follow-up. The host supplies
  model and reasoning choices. Edit, retry, fork and branch checkout need an
  idle, committed boundary.
- `@` picks trusted project files as explicit context. `/` lists host-admitted
  commands, prompt templates, skills and enabled extensions. Commands that need
  interactive extension confirmation aren't available.
- Attach PNG, JPEG, GIF or WebP images, or bounded text, Markdown and ordinary
  PDFs. Image input still depends on the model. The production Serve host
  doesn't accept audio.

The transcript is the main task view. The command center sorts and searches
host-owned task state. Sources, changes, outputs, approvals and progress show up
only when structured evidence supports them. There's no extra agent mode and no
synchronized TUI.

## Models and reasoning

Serve uses the CLI bootstrap catalog, with a 4,096-model safety bound (it
was 256) and the existing bootstrap byte limit. Model availability still depends
on credentials and provider inventory. GPT-6 Sol and Luna aren't injected when
Codex doesn't advertise them. Restart Serve after changing provider
configuration. The installed Serve runtime must also be rebuilt or updated to
the matching source, because updating the CLI alone doesn't update that separate
runtime's discovery code.

The slider shows advertised Ultra as **Ultra**, fading from Max's rainbow to
purple. Ultra still needs advertised Ultra/V2 support and the enabled, trusted,
live `octet-subagents` service, and the UI doesn't bypass those checks. See
[provider reasoning](../../providers.md#reasoning).

## Access and authority

The host binds **IPv4 loopback only**. A one-use launch capability is exchanged
for an ephemeral **HttpOnly, SameSite=Strict** browser cookie before any API or
event-stream access. Host, Origin and Fetch Metadata checks limit requests to
the local app. Keep the launch capability private.

Browser authentication isn't project trust or an agent sandbox. The authority
indicator reflects the host's immutable launch policy. Per-session changes are
unavailable and rejected server-side, so set restrictions before launch and
restart the host to change them. Enabled commands run with your OS authority.
For hostile work, use a restricted user, container, VM or OS sandbox.

**LAN pairing isn't implemented.** There's no working `--lan`, `--demo` or
`--local-only` switch. Don't expose this listener through `0.0.0.0`, a proxy or
port forwarding. The native companion source isn't a supported connection path.
In a source checkout, `apps/ios/README.md` and `apps/macos/README.md` describe
those limitations.

## Terminal and recovery

The terminal appears only when host configuration allows process execution. It
starts a local shell in the workspace and keeps at most four terminals. A
browser disconnect keeps the shell, and host shutdown stops retained shells.
After a reconnect, missing events are replayed, or the state is replaced with an
authoritative snapshot if there's a gap. Accepted queued follow-ups do **not**
survive a host restart. Conversation checkout and forks don't undo filesystem or
other external effects.

<details>
<summary>Terminal cleanup, drafts and deletion</summary>

Closing an inspector or preview only hides it. It doesn't stop anything.
Cleaning up descendants is bounded and isn't OS-level containment. Repeated
command IDs don't run twice. Browser text and attachment drafts are per session
and clear after an acknowledged send.

Archive and trash keep tasks for later access or restore. Permanent deletion
needs the exact confirmation phrase and uses a crash-recovery journal, and
missing required stores fail before commit. Shared payloads and
conversation-content-free inference accounting are kept. See the [deletion and
recovery
notes](../../design/serve-lifecycle-safety.md#permanent-session-deletion). These
come from the source, and recovery hasn't been qualified on the current version.

</details>

<a id="build-install-and-release-gates"></a>

## Install or update a package

With octet `0.8.0`, install or update by name only after matching Serve assets
are published and verified on the exact GitHub release. This checkout doesn't
claim a 0.8.0 publication. Once that gate is met:

```console
octet extension install octet-serve
octet extension update octet-serve
```

For a reviewed, matching local archive instead:

```console
octet extension install --path ./octet-serve-0.8.0-TARGET.tar.gz
octet extension list
octet serve
```

Local archives don't need GitHub network access. The package requires exactly
`=0.8.0`. Replace `TARGET` with `x86_64-unknown-linux-gnu`,
`x86_64-apple-darwin` or `aarch64-apple-darwin`. Linux musl isn't supported. A
local build or archive isn't evidence of signed publication.

Update reinstalls the package that matches the running octet version. It's not
an independent upgrade to a different runtime version.
`octet extension remove octet-serve` removes package files, not Serve sessions
or other user data. See the [package and release
details](../../../extensions/octet-serve/README.md#package-and-release-reference).

<a id="explicit-exclusions"></a>

## Availability limits

Production live previews and child-agent trees are off. Durable source, diff and
output evidence covers successful built-in `read`, `edit` and `write`, not all
Bash or extension changes. There's no arbitrary-folder import from the browser,
no MCP or LSP management, no extension catalog or lifecycle UI, no scheduling,
no WAN access, no multi-host replication and no hosted account service. A
missing capability stays hidden rather than showing up as an empty dashboard
section.

<a id="package-boundary"></a>
<a id="first-web-cut"></a>

## Reference

- [Architecture](architecture.md) and [lifecycle
  safety](../../design/serve-lifecycle-safety.md): technical contracts.
- [Web acceptance](web-acceptance.md) and [provider
  acceptance](provider-acceptance.md): criteria, not a current pass.
- [Project](https://github.com/orgs/skaft-software/projects/5): work tracking.
