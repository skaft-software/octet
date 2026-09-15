# Explicit connector registration

This document is for a host integration that already owns a browser connection.
It is not a browser-discovery API.

## Boundary and defaults

The normal Browse path launches its own visible, isolated Chromium with pinned
Playwright `1.57.0`. An existing browser is opt-in: the host must construct and
register a connector before a request can select it. Browse never scans browser
processes, profiles, sessions, windows, pages, ports, cookies, or storage.

External and isolated browsing are mutually exclusive. Native Firefox and Safari
operations are unsupported; registering a connector whose browser is `firefox`
or `safari` creates an unsupported descriptor and does not substitute Chromium.

## Register a connector

Use `BrowseController.register_backend()` (or pass an `AdapterRegistry` to the
controller) with a `BrowserConnector`, normally `PlaywrightConnector`:

```python
from octet_browse.adapters import PlaywrightConnector


def select_exact(selection, owner):
    # This call belongs to the host integration. It must use every identity
    # supplied by Browse and must not enumerate or guess a target.
    page = owned_bridge.select_exact(
        connector_id=selection.connector_id,
        browser_id=selection.browser_id,
        session_id=selection.session_id,
        window_id=selection.window_id,
        tab_id=selection.tab_id,
        owner=owner,
    )
    return {
        "selection": selection.as_dict(),
        "page": page,
        "browser_family": "chromium",
        "target_revision": owned_bridge.revision(selection, owner),
    }


def verify_exact(target, selection, owner):
    return owned_bridge.verify_exact(selection, target.page, owner)


def release_exact(target, owner):
    owned_bridge.release_exact(target.selection, owner)


def stop_exact(target, owner):
    owned_bridge.stop_exact(target.selection, owner)

connector = PlaywrightConnector(
    "owned-bridge",
    browser="chromium",
    label="Host-owned Chromium bridge",
    selector=select_exact,
    verify_target=verify_exact,
    release=release_exact,
    stop=stop_exact,
    capabilities={
        "snapshot": True,
        "click": True,
        "type": True,
        "press": True,
        "scroll": True,
        "wait": True,
        "screenshot": True,
        "navigation": True,
        "tab_close": True,
        "new_tab": False,
        "popup": False,
    },
)
controller.register_backend(connector)
```

The bridge in this example is an integration-owned object; it is not supplied
by Browse. The callbacks receive the host-derived `ResourceOwner`, not an owner
chosen by the model. They must keep identity and ownership checks in place and
return bounded, generic failures. `selector` must return either a
`PlaywrightTarget` or a mapping containing a Playwright `page` and an exact
identity proof. A returned target with a different identity, browser family,
revision, or closed page is rejected.

`context` may be included in a returned target for integration bookkeeping, but
Browse uses only the selected page and never enumerates other pages in that
context. Do not return credentials, cookies, storage, profile paths, debugging
secrets, or arbitrary evaluation handles.

## Capability declarations

Capabilities describe connector operations, not permission to bypass Browse
policy. Declare only operations the bridge can perform safely. Capability checks
happen before confirmation and before the browser call:

- `navigation` is required to navigate an already-selected tab.
- `new_tab` is required when navigation would create a tab.
- `popup` is required for connector-owned popup pages.
- `snapshot`, `click`, `type`, `press`, `scroll`, `wait`, `screenshot`, and
  `tab_close` gate their corresponding operations.
- `window_resize`, `cookies`, and `storage` are recognized as unsupported
  Browse surfaces and must not be enabled as a way around policy.

The `type` capability does not permit automated authentication. Browse still
refuses credential-like fields and never returns the supplied value. Click and
consequential key actions still require action-time confirmation. Navigation
remains HTTP(S)-only and downloads remain blocked.

## Exact selection and lifecycle

The four backend lifecycle tools are separate from ordinary browser tools:

- `browser_backend_select` requires `connector_id`, `browser_id`, `session_id`,
  `window_id`, and `tab_id`; `target_revision` is optional.
- `browser_backend_status` reports bounded connector descriptors and state, not
  ambient browser inventory or unrelated target details.
- `browser_backend_revoke` verifies and releases the exact selected target.
- `browser_backend_stop` requires an explicit `stop` callback, invokes it, and
  then releases the selected target. Stop behavior is never inferred from a
  process or page state.

Selection claims the complete target identity for the host-derived owner before
the connector selector runs. A target revision, page-liveness check, and optional
connector verifier are rechecked before connector-backed operations. A second
owner cannot inspect or operate a claimed target. Revoke the selected target
before unregistering its connector; the registry rejects unregistering a claimed
connector. Connector registration is bounded to 16 descriptors.

After shutdown or an owner change, release any integration-owned resources from
the connector lifecycle callbacks. Do not use connector registration as a way
to transfer an existing browser session between owners.

## Native Firefox/Safari prerequisites (#378)

Native operation remains **unsupported**, not silently mapped onto Chromium.
`NativeFirefoxConnector` and `NativeSafariConnector` are descriptors only. The
current worker consumes a Playwright page for locator metadata, reference
lifetimes, actions, navigation and screenshots; changing a family allowlist is
not a native implementation. Playwright's patched Firefox and WebKit builds are
not an attachment bridge to the user's existing Firefox or Safari tabs.

A safe complete native connector first needs all of the following:

1. A reviewed, versioned native transport supplied and explicitly enabled by the
   host/user, with installation/permission and revocation UX. No process/port
   scanning, normal-profile reuse, or implicit remote-automation enablement.
   A Firefox WebExtension/native-messaging bridge or a Safari-specific bridge
   must demonstrate existing-target operation; a driver that creates a separate
   automation session does not satisfy existing-tab selection.
2. A trusted picker or integration that issues the complete connector/browser/
   session/window/tab identity and a revision/liveness proof. No title matching,
   active-tab fallback, integer tab-index guessing, or selecting a different
   page after closure, navigation, window moves, process restart or owner change.
3. An operation adapter (not a fabricated Playwright page) covering each declared
   capability, with serialized bounded calls, exact-target verification before
   every action, stable snapshot-generation fencing and untrusted observations.
   Native errors must not expose transport secrets, profile paths or credentials.
4. Proven safety at the selected target: credential/payment/form metadata without
   reading values; manual authentication; action-time consequential confirmation;
   pre-navigation/redirect/popup HTTP(S) enforcement; download cancellation; and
   conservative viewport screenshot refusal around possible sensitive values.
   Native APIs that cannot enforce a boundary must leave that capability disabled.
   The existing external selection path only installs a download listener; the
   isolated context's route interception is not automatically transferred to a
   native connector. Post-navigation URL inspection alone is not prevention.
5. Exact-target release/stop, owner-change cleanup and takeover behavior that
   never closes or operates unrelated windows/tabs. Test denied/revoked grants,
   stale identities, wrong owners, tab replacement, transport loss and timeout,
   then qualify on an explicitly authorized real Firefox/Safari target with
   unrelated windows present. No such native campaign has run for this candidate.

The dependency-free native refusal/identity/owner regressions live in
`tests/test_adapters.py`. They prove the unsupported boundary stays closed, not
that native browsing works. Supplying these prerequisites is separate from the
[isolated Chromium focus candidate](QUALIFICATION.md).
