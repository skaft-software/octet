# Changelog

## [Unreleased]

- Sign in to paid plans without a long-lived API key, the way Codex sign-in
  already works. `--login grok`, `--login kimi`, `--login meta`, and
  `--login openrouter` each run the provider's own OAuth grant and store one
  owner-private credential under `~/.octet/credentials/`;
  `--logout <provider>` removes only that credential. Grok, Kimi, and Meta use a
  device code and honor `--headless`, so they work over SSH; OpenRouter's browser
  login asks you to paste the redirect URL your browser lands on. Each is a
  separate provider from the same vendor's API-key preset
  (`xai-subscription/…` alongside `xai/…`), so signing in never replaces a key you
  configured, and a provider's models appear only while you are signed in.
  Meta's grant is two steps — the device flow yields an identity token, which is
  exchanged for a short-lived API key — and OpenRouter mints a durable key that
  octet never renews. Refresh-token rotation is serialized within and across
  octet processes and re-checks the credential after taking the cross-process
  lock, so two concurrent launches cannot spend the same single-use token.
  Anthropic (Claude Pro/Max) is deliberately not included: it needs a request-path
  change in the Anthropic Messages codec before a privately resolved OAuth
  credential would receive the beta headers a subscription token requires.
  `ANTHROPIC_AUTH_TOKEN` and `ANTHROPIC_OAUTH_TOKEN` are unchanged.

