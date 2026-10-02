# octet-web-search

**Source candidate distribution: 0.8.2.** This bundle needs exactly octet 0.8.2.
Use the [version-matched installation](../../docs/installation.md) and the
[0.8.2 candidate record](../../docs/releases/v0.8.2.md) for availability and
remaining qualification. 0.8.2 assets are not published; use the reviewed
source-checkout route below until publication is approved.

Search the public web and retrieve pages with stable citations. Pick [Brave
Search](https://brave.com/search/api/) or a configured
[SearXNG](https://docs.searxng.org/) JSON endpoint. It doesn't open browser
tabs, sign in, run JavaScript or submit forms.

## Start a search

With [octet 0.8.2](../../docs/installation.md), Python 3.9+ available as
`python3`, and verified matching published assets, the catalog path is:

```console
octet extension install octet-web-search
octet --enable-extension octet-web-search
```

For a reviewed source checkout instead, add `--extension-dir ./extensions` to
the launch command from the repository root.

Then open `/extensions`, choose **octet-web-search** (choosing a disabled
extension enables it first), and pick **Use Brave Search**. Load the optional
research skill with `/skills load octet-web-search`.

Brave setup shows <https://api.search.brave.com/app/keys> and asks for the key
through a private input. Don't paste a key into a prompt or ordinary config.
Then ask something like: "Find the official Python pathlib documentation and
cite the sources for your summary."

Pick **Use SearXNG** instead for SearXNG. Its instance must allow `format=json`,
and **Change the SearXNG endpoint** points it elsewhere later. The same menu
shows the current provider, checks its status, logs out of Brave Search
(deleting the stored key) and disables the extension. The web UI has no options
menu yet. There the same actions run as the `/web-search` command (`status`,
`setup brave`, `setup searxng`, `endpoint`, `logout`).

## What the tools do

| Tool | Use | Hard limits |
| --- | --- | --- |
| `web_search` | Search with the selected provider. | 512-byte query, 5 requested domains, 10 results, 20 seconds, 512 KiB provider response. |
| `web_fetch` | Fetch one public HTML, XHTML or plain-text page. | HTTP(S) ports 80 and 443, 20 seconds, 3 redirects, 4 MiB download, 128 KiB normalized content. |
| `web_find` | Find a literal pattern and return excerpts. | 256-byte pattern, 20 matches, 512-byte excerpts, plus the fetch limits. |

Config and call arguments can lower the limits, never raise them. The
`max_bytes` tool argument limits normalized output, not the HTTP download.
Existing configs with a smaller `limits.max_download_bytes` keep that lower cap
until updated. Cite the returned `[web-…]` IDs. They come from sanitized URLs,
not result rank or cache state. Text results are marked **UNTRUSTED WEB DATA**,
and their content can't grant permission or change policy.

## Privacy and configuration

Queries and domain filters go to your search provider. Fetch and find send the
sanitized URL to the public origin, with normal DNS and TLS traffic. Queries and
retrieved content stay in ordinary tool arguments and results, not in compact
status or activity labels. The cache is bounded, in-process and never written to
disk.

Brave credentials live in the owner-private file
`~/.octet/credentials/octet-web-search-brave.key`. They never appear in URLs,
results, diagnostics or frontend state. Credentialed requests never redirect,
and a 401 or 403 invalidates the stored key so setup or search can ask again.

SearXNG settings live in `~/.config/octet/octet-web-search.json`, and the
provider picker keeps them while Brave is selected. Endpoint URLs must be
non-secret. Configured query parameters such as `timeout_limit` are kept, and
the search request adds its own query, JSON and safe-search parameters. A
private self-hosted provider needs `allow_private_endpoint: true`, and that
never allows private `web_fetch` or `web_find` destinations or redirects.
`limits.allowed_domains` is an egress allowlist, and a tool's `domains` can only
narrow it. See the [full configuration rules](REFERENCE.md#searxng).

## Activation and reference

Installing is inert, and the bundle stays disabled until you enable it. Default
full access (`unsafe_host`) trusts the selected extension implicitly, without
saving a grant. `--trust-extension` and source-bound `trusted_extensions` grants
are optional, and are never activation. `--safe-mode` removes implicit trust and
keeps the process stopped even with explicit grants: executable startup still
needs `unsafe_host`. An admitted extension has your OS authority, and manifest
consent metadata isn't a sandbox. Skill loading is independent.

The source bundle `0.8.2` needs exactly octet `0.8.2`. For a reviewed source
checkout, select it with `--extension-dir ./extensions`. What follows is a
bundled-runtime reference, not a general SDK authoring tutorial.

- <a id="install-and-opt-in"></a>[Install and opt
  in](REFERENCE.md#install-and-opt-in): public catalog installation and
  persistent activation.
- <a id="choose-a-provider"></a>[Choose a
  provider](REFERENCE.md#choose-a-provider).
  - <a id="brave-search-recommended"></a>[Brave Search
    (recommended)](REFERENCE.md#brave-search-recommended).
  - <a id="searxng"></a>[SearXNG](REFERENCE.md#searxng): full example and strict
    file validation.
- <a id="tools-and-bounds"></a>[Tools and
  bounds](REFERENCE.md#tools-and-bounds): normalization, citations and
  address-pinned egress.
- <a id="trust-egress-and-result-visibility"></a>[Trust, egress, and result
  visibility](REFERENCE.md#trust-egress-and-result-visibility).
- <a id="cache-cancellation-health-and-offline-behavior"></a>[Cache,
  cancellation, health, and offline
  behavior](REFERENCE.md#cache-cancellation-health-and-offline-behavior).
- <a id="frontend-neutral-presentation"></a>[Frontend-neutral
  presentation](REFERENCE.md#frontend-neutral-presentation): retained-state
  privacy and reconnect behavior.
- <a id="test"></a>[Test](REFERENCE.md#test): fixture coverage, not
  live-provider qualification.
