# octet themes

Theme-file customization is disabled. octet uses one compiled default theme
whose model-family accents adapt focused controls and startup atmosphere without
changing layout or semantic status colours.

On the first capable interactive TUI launch, octet offers three built-in
terminal appearances:

- `Auto (recommended)` detects the terminal background through reliable
  environment or terminal capability signals and uses a readable neutral
  fallback when detection is unavailable.
- `Light terminal` and `Dark terminal` explicitly select the corresponding
  contrast profile and override detection.

Moving through the picker previews each appearance without saving it. Confirming
persists `theme = "auto"`, `theme = "light"`, or `theme = "dark"` in the user
config without replacing unrelated settings. Use `/theme` later to revisit it;
`/theme auto`, `/theme light`, and `/theme dark` are also accepted. Cancelling
`/theme` restores the previous appearance; dismissing first-run onboarding uses
Auto. Existing configured installations do not reopen onboarding, and
print/plain/RPC, redirected, and `TERM=dumb` sessions never open it.

The built-in choices also work with `--theme` and `OCTET_THEME`. Other theme names,
`--theme-dir`, and arbitrary theme files remain compatibility inputs only. They
never add a theme loader or marketplace; unrecognized values fall back to the
compiled default. An explicit
`OCTET_COLOR_SCHEME` is also treated as an existing terminal-appearance choice,
so automation and already-configured shells do not get interrupted by
onboarding.
