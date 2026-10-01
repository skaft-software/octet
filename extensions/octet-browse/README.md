# octet Browse

**Distribution: 0.8.2.** This bundle needs exactly octet 0.8.2. Use the
[version-matched installation](../../docs/installation.md) and the [0.8.2
release record](../../docs/releases/v0.8.2.md) for signed assets and
public-install evidence.

A visible, isolated Chromium window for inspecting pages and taking bounded
browser actions. You sign in manually. octet Browse never uses your normal
browser profile.

<a id="deprecation"></a>

## Deprecation

**This bundle is deprecated. It still works and stays installable, but don't
build new automation on it.**

The replacement is the computer-use extension, which drives a locally installed
[Cua Driver](https://github.com/trycua/cua) (MIT) to operate native applications
on macOS, Windows and Linux.

Browse stays, instead of being removed, because it still gives a safety property
the replacement doesn't: a **visible, isolated, octet-owned Chromium profile
with a manual-authentication boundary**. The agent never touches your real
browser, its cookies or your logged-in sessions. The computer-use path drives
your actual desktop, including any browser you already have open, so
authenticated page work there has a materially different risk profile.

- **New, unauthenticated automation:** prefer the computer-use extension.
- **Anything involving a login, account or saved session:** Browse is still the
  safer choice today.
- Browse isn't scheduled for removal. It will be retired only after the
  computer-use path demonstrably covers the isolated-browser case.

<a id="start-from-a-reviewed-checkout"></a>

## Install the bundle

With [octet 0.8.2](../../docs/installation.md) and verified matching published
assets, the catalog path is:

```console
octet extension install octet-browse
octet --enable-extension octet-browse
```

For a reviewed source checkout instead, add `--extension-dir ./extensions` to
the launch command from the repository root.

Then open `/extensions`, choose **octet-browse** (choosing a disabled extension
enables it first), and pick **Set up the browser**. Setup asks before
downloading the pinned Playwright dependencies and Chromium, then shows each
step live, including the Chromium download. Esc stops watching, but the install
keeps running in the background and the menu shows its state. When the menu says
**Ready**, pick **Open the browser**, then load the skill with
`/skills load octet-browse`. Then ask something like: "Open https://example.com
and summarize the visible page. Do not submit forms."

The bundle stays disabled until you enable it. Default full access
(`unsafe_host`) trusts it implicitly, without saving a grant.
`--trust-extension` and source-bound `trusted_extensions` grants are optional,
and never activation. Safe mode removes implicit trust and keeps it stopped even
with explicit grants: executable startup still needs `unsafe_host`. It runs with
your OS authority, not in a sandbox. Installing files starts nothing, and skill
activation is separate.

## Use an explicitly injected browser connector

Isolated, visible Chromium is the default. Connecting to an already-running
browser is opt-in and needs a host integration to register a connector first.
Browse never discovers browsers, lists processes, attaches to normal profiles or
substitutes a native Firefox or Safari backend. The model must give the exact
connector, browser, session, window and tab identities, and omitted or stale
identities fail closed.

<details>
<summary>Connector rules</summary>

Connector-backed browsing is mutually exclusive with the isolated browser. The
same owner fencing, stale-target checks, capability checks, manual-auth
boundary, bounded results and cleanup rules apply. External actions are off by
default and need a connector that installs and verifies a preventive navigation,
popup and download boundary. Checking the selected page's URL after an action is
not prevention. The isolated Chromium route policy isn't inherited by external
targets. Connector integrations must not expose credentials, cookies, storage,
profile paths or ambient browser discovery. See [connector
registration](CONNECTORS.md) for the host contract and [the
reference](REFERENCE.md#explicit-connectors) for tool behavior.

</details>

## Work safely

- Sign-in is manual, in the visible window. The typing tool refuses credential,
  OTP, authentication and payment fields, and keeps supplied text out of its
  logs and errors.
- Purchase, publish, send, consent, external-side-effect and delete actions ask
  for confirmation. Denial, cancellation, timeout or no interactive confirmation
  fails closed. Page text can't authorize an action.
- Browser observations are untrusted data, not instructions. Only explicit
  absolute HTTP(S) navigation is allowed. Downloads are cancelled.
- Tabs have explicit IDs. Snapshot refs expire on navigation or a newer
  snapshot, and ambiguous targets fail rather than picking the first match.
- Screenshots are viewport-only and refuse anything that might expose a form
  value. There's no JavaScript, clipboard, file-transfer, cookie, storage or
  normal-profile access.
- Repeated open requests reuse the existing visible window. If you close it or
  it crashes, Browse reports a degraded closed state and releases its owned
  helpers, and only a new explicit open request may relaunch it. Tool-created
  tabs ask for non-activating creation in that visible browser. The initial
  launch and page-created popups can still take focus. Physical focus
  preservation isn't qualified yet (see [qualification](QUALIFICATION.md)).

Pick **Close the browser** in the same menu when you're done. **Reset the
browser profile** asks again before removing only the locked, sentinel-verified
isolated profile. The web UI has no options menu yet. There the same actions run
as the `/browse` command (`setup`, `status`, `open`, `close`, `reset-profile`).

## Reference

The bundled runtime uses API `0.4`, so these are usage and implementation
references, not general extension-authoring tutorials. Bundle `0.8.2` needs
exactly octet `0.8.2` and `playwright==1.57.0`.

- <a id="install-and-activate"></a>[Install and
  activate](REFERENCE.md#install-and-activate): inert installation, persistent
  activation and skill readiness.
- <a id="commands"></a>[Commands](REFERENCE.md#commands): setup, status, open,
  close and reset.
- <a id="tool-surface"></a>[Tool surface](REFERENCE.md#tool-surface): all 17
  tools, targets, owner fencing, keys and limits.
- <a id="explicit-connectors"></a>[Explicit
  connectors](REFERENCE.md#explicit-connectors): host-registered
  existing-browser targets and lifecycle tools.
- <a id="connector-registration"></a>[Connector registration](CONNECTORS.md):
  the explicit injected-connector contract.
- <a id="authentication-and-actions"></a>[Authentication and
  actions](REFERENCE.md#authentication-and-actions): confirmation and navigation
  policy.
- <a id="untrusted-observations-and-screenshots"></a>[Untrusted observations and
  screenshots](REFERENCE.md#untrusted-observations-and-screenshots): redaction,
  retention and the limited form-screenshot override.
- <a id="owned-state-and-cleanup"></a>[Owned state and
  cleanup](REFERENCE.md#owned-state-and-cleanup): paths, locks, worker ownership
  and shutdown.
- <a id="development-and-tests"></a>[Development and
  tests](REFERENCE.md#development-and-tests): documented local test commands,
  not live-browser qualification.
- <a id="license"></a>[License](REFERENCE.md#license).
