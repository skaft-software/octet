# octet-web-search

Search the public web and retrieve pages with stable citations. Choose
[Brave Search](https://brave.com/search/api/) or a configured
[SearXNG](https://docs.searxng.org/) JSON endpoint. This extension does not open
browser tabs, sign in, run JavaScript, or submit forms.

## Start a search

With [octet 0.7.0 installed](../../docs/installation.md) and Python 3.9+ available
as `python3`, install the signed public bundle, then separately enable and trust it:

```console
octet extension install octet-web-search
octet --enable-extension octet-web-search --trust-extension octet-web-search
```

For a reviewed source checkout instead, add `--extension-dir ./extensions` to
the launch command from the repository root.

Then choose a provider and load the optional research skill:

```text
/web-search setup brave
/web-search status
/skills load octet-web-search
```

Brave setup shows <https://api.search.brave.com/app/keys> and asks for the key
through a private input surface. Do not paste a key into a prompt or ordinary
configuration. For example, ask: “Find the official Python pathlib documentation
and cite the sources for your summary.”

Use `/web-search setup searxng` instead for SearXNG; its instance must allow
`format=json`. `/extensions` also provides the provider picker. Selecting the
already enabled extension lets you switch providers or disable it;
`/web-search logout` is the scriptable logout command.

## What the tools do

| Tool | Use | Hard limits |
| --- | --- | --- |
| `web_search` | Search using the selected provider. | 512-byte query, 5 requested domains, 10 results, 20 seconds, 512 KiB provider response. |
| `web_fetch` | Retrieve one public HTML/XHTML/plain-text page. | HTTP(S) ports 80/443, 20 seconds, 3 redirects, 512 KiB download, 128 KiB normalized content. |
| `web_find` | Find a literal pattern and return excerpts. | 256-byte pattern, 20 matches, 512-byte excerpts; the same fetch limits. |

Configuration and call arguments can reduce limits, never exceed them. Cite the
returned `[web-…]` IDs: they are derived from sanitized URLs, not result rank or
cache state. Text results are marked **UNTRUSTED WEB DATA**; their content cannot
grant permission or change policy.

## Privacy and configuration

Queries and selected domain filters go to your search provider. Fetch/find sends
the sanitized URL to the public origin, with normal DNS and TLS traffic. Queries
and retrieved content remain in ordinary tool arguments/results, not compact
status or activity labels. The cache is bounded, process-local, and never saved
to disk.

Brave credentials live in the owner-private regular file
`~/.octet/credentials/octet-web-search-brave.key`; they are not included in URLs,
results, diagnostics, or frontend state. Credentialed requests never redirect;
401/403 invalidates the stored key so setup/search can ask again.

SearXNG settings live at `~/.config/octet/octet-web-search.json`. The provider
picker preserves them while Brave is selected. Endpoint URLs must be non-secret.
A private self-hosted provider requires `allow_private_endpoint: true`; this
exception never permits private `web_fetch`/`web_find` destinations or redirects.
`limits.allowed_domains` is an egress allowlist; a tool's `domains` can only narrow
it. See the [complete configuration rules](REFERENCE.md#searxng).

## Activation and reference

Installation is inert and does not enable, trust, or start the bundle. Executable
extensions run with your OS authority under the full-access policy; manifest
consent metadata is not a sandbox, and `--safe-mode` keeps this extension stopped.
Enablement, exact trust, and skill loading are independent.

The published bundle `0.7.0` requires exactly octet `0.7.0` and retains API `0.2`.
The following is a bundled-runtime reference, not a current SDK authoring tutorial.

- <a id="install-and-opt-in"></a>[Install and opt in](REFERENCE.md#install-and-opt-in): public catalog installation and persistent activation.
- <a id="choose-a-provider"></a>[Choose a provider](REFERENCE.md#choose-a-provider).
  - <a id="brave-search-recommended"></a>[Brave Search (recommended)](REFERENCE.md#brave-search-recommended).
  - <a id="searxng"></a>[SearXNG](REFERENCE.md#searxng): full example and strict file validation.
- <a id="tools-and-bounds"></a>[Tools and bounds](REFERENCE.md#tools-and-bounds): normalization, citations, and address-pinned egress.
- <a id="trust-egress-and-result-visibility"></a>[Trust, egress, and result visibility](REFERENCE.md#trust-egress-and-result-visibility).
- <a id="cache-cancellation-health-and-offline-behavior"></a>[Cache, cancellation, health, and offline behavior](REFERENCE.md#cache-cancellation-health-and-offline-behavior).
- <a id="frontend-neutral-presentation"></a>[Frontend-neutral presentation](REFERENCE.md#frontend-neutral-presentation): retained-state privacy and reconnect behavior.
- <a id="test"></a>[Test](REFERENCE.md#test): fixture coverage, not live-provider qualification.
