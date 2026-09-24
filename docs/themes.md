# octet themes

octet's compiled default theme retains model-family accents,
terminal-background adaptation, and semantic status colours. Auto, Dark, and
Light share the same default geometry: 16×4 model-blended startup mark, compact
per-line historical prompt highlights preserving inline Markdown styles,
model-coloured composer rules, one blinking tool-like subagent transcript row
while workers run, two-row command previews, and full available prose width
without narrowing code or tables. The footer may
omit a redundant catalogue-owned provider prefix, never a configured model
name. These are compiled presentation policies, not new theme-file fields:
custom surfaces, composer frames, and loaded theme colours/geometry keep their
own styling.
Unknown-background and no-colour terminals retain an unpainted readable prompt.

The built-in theme picker offers:

- `Auto (recommended)` detects the terminal background through reliable
  environment or terminal capability signals and uses a readable neutral
  fallback when detection is unavailable.
- `Light terminal` and `Dark terminal` explicitly select the corresponding
  contrast profile and override detection.

Moving through the picker previews each appearance without saving it. Confirming
persists `theme = "auto"`, `theme = "light"`, or `theme = "dark"` in the
user config without replacing unrelated settings.
Use `/theme` later to revisit it. Cancelling
`/theme` restores the previous appearance; dismissing first-run onboarding uses
Auto. Existing configured installations do not reopen onboarding, and
print/plain/RPC, redirected, and `TERM=dumb` sessions never open it.

The built-in choices also work with `--theme` and `OCTET_THEME`. Other theme
names, `--theme-dir`, and arbitrary theme files are accepted only through the
bounded file loader: a name that resolves through normal resource discovery
(global `~/.octet/themes`, a trusted project `.octet/themes`, or `--theme-dir`)
is loaded at startup when named by `--theme`/`OCTET_THEME`, while the
interactive `/theme` command accepts only `auto`, `light`, and `dark`.
There is no theme marketplace, and an unrecognized or malformed name falls back
to the compiled default. An explicit
`OCTET_COLOR_SCHEME` is also treated as an existing terminal-appearance choice,
so automation and already-configured shells do not get interrupted by
onboarding.

## Activity status contrast

`Thinking` and `Working` use model-family foreground colours only; the shimmer
never paints a character background. On known Dark/Light TrueColor or ANSI256 profiles,
the default physical shimmer uses a raised-cosine light field: it has a short
leading edge, a longer trailing tail, and a small central glint. Dark labels move
from about `0.55` toward `0.99` luminance; light labels move from about `0.0085`
toward `0.11`. The motion completes in roughly 0.85–1.0 seconds and then parks
for two ticks before repeating. The margin dot shares that phase without changing
its glyph or size.

Set `OCTET_SHIMMER=classic` to use the legacy stepped shimmer for A/B testing.
Classic is also forced for ANSI16, unknown-background, no-colour, and reduced-motion
profiles. An unrecognized variable value uses the physical default when the
terminal profile supports it. The qualification fixture checks representative
Dark/Light composited surfaces; it does not measure arbitrary transparency.
Custom ANSI16 palettes can also change perceived contrast.

## Startup and terminal replies

The first branded frame waits for resolved model/setup state, workspace, and
appearance; the welcome animation starts at that boundary. SSH alone does not
reduce the advertised terminal color capability.

Auto can issue one OSC 11 background query and use a neutral fallback after its
short detection deadline. The shared input owner continues recognizing a late
reply after that deadline, including slowly fragmented bodies once the OSC 11
header is recognized. Genuine typing and bracketed paste are retained.

Escape and Alt+] remain genuine keys: an incomplete opening header has a 250 ms
ambiguity timeout. A header fragmented more slowly can still pass through as
input. Use explicit `--theme dark` or `--theme light` to skip the query on such
terminals. This is not a guarantee for arbitrary terminal-protocol corruption.

## Variant reference

octet ships a complete reference file for the compiled default theme at
[`examples/themes/octet-default.toml`](../examples/themes/octet-default.toml).
It is not read at runtime — the compiled default is always the fallback — but it
documents the accepted sections in one place and is a valid starting point:

```console
cp examples/themes/octet-default.toml ~/.octet/themes/mine.toml
```

A theme file is a bounded TOML document (256 KiB) with these typed sections:

- `[metadata]` — `name`, `description`, `author`, `version`, `terminal`
  (`light-dark`, `dark`, `light`, or `any`), and optional `adaptive` to rebalance
  RGB foregrounds and surfaces for the detected terminal background.
- `[colors]` / `[tokens]` — flat or nested colour/token values. `"default"`
  means the terminal's own colour.
- `[roles.<name>]` — per-role `foreground`, `background`, `bold`, `dim`,
  `italic`, `underline`, `strikethrough`, `inverse`, and `adaptive`.
- `[glyphs]` / `[glyphs_ascii]` — typed glyphs; every structural glyph is one
  column and ASCII fallbacks must be ASCII.
- `[surfaces.<kind>]` — bounded layout recipes for the `user`, `assistant`,
  `reasoning`, `tool`, `notice`, `outcome`, `shell`, and `compaction` transcript
  surfaces.
- `[layout]` — density, shell visibility, and narrow-terminal overrides.
- `[variants.*]` — background overlays described below.

`[variants.universal]`, `[variants.dark]`, and `[variants.light]` merge
recursively over the base document using the same section shapes. `universal`
applies to every terminal; `dark`/`light` then overlay it for the detected
background profile, so one variant may override a single token without
restating the whole table. `[variants.unknown]` applies when detection fails.

Theme files reject terminal control bytes, unknown sections and fields, invalid
role names, non-ASCII ASCII-fallbacks, wide or empty structural glyphs, and
oversized values. Unknown or partial files fall back to the compiled default
rather than starting with a broken shell.

## Semantic role vocabulary

`[roles.<name>]` is typed. These names are the published, terminal-independent
vocabulary that maps onto octet's semantic text roles; the
`published_semantic_role_vocabulary_is_closed_and_accepted` test in
`crates/octet-coding-agent/src/tui/theme.rs` keeps this list and
`SEMANTIC_ROLE_VOCABULARY` in sync:

`text`, `foreground`, `muted`, `subtle`, `dim`, `accent`, `success`, `warning`,
`error`, `heading`, `md_heading`, `emphasis`, `md_emphasis`, `strong`,
`md_strong`, `inline_code`, `md_code`, `code`, `md_code_block`, `quote`,
`md_quote`, `border`, `link`, `md_link`, `list_marker`, `md_list_bullet`,
`diff_add`, `diff_added`, `diff_remove`, `diff_removed`, `diff_context`,
`diff_hunk`, `diff_header`, `syntax_comment`, `syntax_keyword`,
`syntax_function`, `syntax_variable`, `syntax_string`, `syntax_number`,
`syntax_type`, `syntax_operator`, `syntax_punctuation`.

Alias spellings (`foreground`/`text`, `dim`/`subtle`, `code`/`md_code_block`,
`diff_add`/`diff_added`, and so on) resolve to the same underlying role.

## Extension-contributed themes and roles

Two channel types are open to extensions:

- **Roles.** Any name of the form `extension.<namespace>.<role>` is accepted in
  `[roles]` (up to 96 bytes, ASCII alphanumerics plus `_`, `-`, and `.`). The
  schema is open but typed: an extension may add
  `[roles."extension.git.branch"]` without a host change, but it cannot inject
  the same name under a private, unnamespaced key. This is tested by
  `semantic_extension_roles_are_open_but_typed` in `theme_schema.rs`.
- **Theme files.** An extension package may ship `themes/<name>.toml`. Install
  or copy it into a discovery root so the shared resolver can select it:
  `~/.octet/themes/` (global), `.octet/themes/` (project, requires trust), or a
  directory passed with `--theme-dir`. Discovered files use the same bounded,
  no-follow reader as the compiled default and never execute extension code.

A manifest-level `contributes.themes` channel that would register an
extension's own directory as a theme root is **not implemented**; it requires a
change in the extension manifest schema and discovery (outside the theme
module). Until then, publishing a theme file into a discovery root is the
supported contribution path.
