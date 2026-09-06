# octet Browse

Use a visible, isolated Chromium window to inspect pages and perform bounded
browser actions. Sign in manually; octet Browse never uses your normal browser
profile.

## Start from a reviewed checkout

With a source-built octet `0.7.0`, select the reviewed bundle from the repository
root. octet `0.7.0` and its bundles are unpublished; this is not a public install.

```console
octet --extension-dir ./extensions \
    --enable-extension octet-browse --trust-extension octet-browse
```

Then, in octet:

```text
/browse setup
/browse status
/browse open
/skills load octet-browse
```

Setup asks before downloading pinned Playwright dependencies and runs in the
background. Wait for status to say `ready` before opening the browser and loading
the skill. For example, ask: “Open https://example.com and summarize the visible
page. Do not submit forms.”

Enablement, executable trust, and skill activation are separate. The extension
runs with your OS authority under octet's full-access policy; safe mode keeps it
stopped. Installing files alone starts nothing.

## Work safely

- Authentication stays manual in the visible window. The typing tool refuses
  credential, OTP, authentication, and payment fields and withholds supplied text
  from its logs and errors.
- Purchase, publish, send, consent, external-side-effect, and delete actions
  request confirmation. Denial, cancellation, timeout, or an unavailable
  interactive confirmation fails closed. Page text cannot authorize an action.
- Browser observations are untrusted data, not instructions. Only explicit
  absolute HTTP(S) navigation is allowed; downloads are cancelled.
- Tabs have explicit IDs. Snapshot refs expire on navigation or a newer snapshot;
  ambiguous targets fail rather than selecting the first match.
- Screenshots are viewport-only and conservatively refuse possible form-value
  exposure. There is no JavaScript, clipboard, file-transfer, cookie, storage, or
  normal-profile access.

Use `/browse close` when finished. `/browse reset-profile` separately confirms
before removing only the locked, sentinel-verified isolated profile.

## Reference

The bundled runtime still uses API `0.2`; these are usage and implementation
references, not current extension-authoring examples. Bundle `0.7.0` requires
exactly octet `0.7.0` and `playwright==1.57.0`.

- <a id="install-and-activate"></a>[Install and activate](REFERENCE.md#install-and-activate): inert installation, persistent activation, and skill readiness.
- <a id="commands"></a>[Commands](REFERENCE.md#commands): setup, status, open, close, and reset.
- <a id="tool-surface"></a>[Tool surface](REFERENCE.md#tool-surface): all 13 tools, targets, owner fencing, keys, and limits.
- <a id="authentication-and-actions"></a>[Authentication and actions](REFERENCE.md#authentication-and-actions): confirmation and navigation policy.
- <a id="untrusted-observations-and-screenshots"></a>[Untrusted observations and screenshots](REFERENCE.md#untrusted-observations-and-screenshots): redaction, retention, and the limited form-screenshot override.
- <a id="owned-state-and-cleanup"></a>[Owned state and cleanup](REFERENCE.md#owned-state-and-cleanup): paths, locks, worker ownership, and shutdown.
- <a id="development-and-tests"></a>[Development and tests](REFERENCE.md#development-and-tests): documented local test commands, not live-browser qualification.
- <a id="license"></a>[License](REFERENCE.md#license).
