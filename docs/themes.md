# octet themes

octet's compiled default theme retains model-family accents,
terminal-background adaptation, and semantic status colours. Auto, Dark, and
Light share the same default geometry: 16×4 model-blended startup mark, a
full-cell model-adaptive card per historical prompt (the stored colour of the
model that received it fills the whole cell, cushion rows and inline Markdown
styling included), model-coloured composer rules, one blinking tool-like
subagent transcript row while workers run, two-row command previews, and full
available prose width without narrowing code or tables. The footer may
omit a redundant catalogue-owned provider prefix, never a configured model
name. These are compiled presentation policies, not new theme-file fields:
custom surfaces, composer frames, and loaded theme colours/geometry keep their
own styling.
Unknown-background and no-colour terminals retain an unpainted readable prompt.

A theme that sets `content_max_width` gets a centered reading column for its
text, but its filled chrome still reaches the terminal edges: a shaded composer
and borderless `band`/`rail` surfaces paint edge to edge rather than floating as
a narrow island with unpainted bars beside them. Bordered cards, plain text, and
themes that only inset their content without capping the column are unchanged.

The first-run appearance picker offers:

- `Auto (recommended)` detects the terminal background through reliable
  environment or terminal capability signals and uses a readable neutral
  fallback when detection is unavailable.
- `Light terminal` and `Dark terminal` explicitly select the corresponding
  contrast profile and override detection.

Moving through the picker previews each appearance without saving it. Confirming
persists `theme = "auto"`, `theme = "light"`, or `theme = "dark"` in the
user config without replacing unrelated settings. Dismissing first-run onboarding
uses Auto. Existing configured installations do not reopen onboarding, and
print/plain/RPC, redirected, and `TERM=dumb` sessions never open it.

Use `/theme` later to browse the built-in choices **and** valid `.toml` files
from normal resource discovery (global `~/.octet/themes`, a trusted project
`.octet/themes`, and directories or files passed with `--theme-dir`). The list
shows the file stem and optional metadata name/description; typing filters it.
Selecting a file previews its appearance and confirming persists its file stem
as `theme = "mine"` in the user config, keeping other settings intact.
`/theme mine` (or `/theme mine.toml`) selects a discovered, valid file directly.
`/theme Cards` and `/theme Still` select the compiled-in file themes described
below.
Cancelling restores the active theme without modifying the config or session.
The first-run picker still shows only Auto, Light, and Dark.

Discovery follows global < trusted project < explicit path precedence; duplicate
file stems show only the winning file, even when that file is invalid. Files that
fail bounded reads or schema validation are omitted from the interactive list.
In the interactive picker, file stems `auto`, `light`, `dark`, `default`,
`Cards`, and `Still` are reserved for built-in selectors and do not appear as
file choices.
The built-in choices also work with `--theme` and `OCTET_THEME`; named files may
be loaded at startup by those selectors. There is no theme marketplace. An
unrecognized or malformed theme name at startup falls back to the compiled
default. An explicit `OCTET_COLOR_SCHEME` is also treated as an existing
terminal-appearance choice, so automation and already-configured shells do not
get interrupted by onboarding.

## Prompt provenance

Every submitted prompt stores the exact colour of the model that received it, so
a session keeps its provenance after a model switch or a restart. The default
theme turns that stored colour into a full-cell card: the background covers the
whole row — marker gutter, padding, trailing canvas, and the breathing rows above
and below — and the rich renderer's own bold/italic/inline-code runs are layered
inside the card rather than flattened onto one colour. Unknown-background and
no-colour terminals leave the prompt unpainted on the terminal canvas.

One top-level token changes that:

- `prompt_wash` — `false` keeps prompt rows on the surface's own fill (a
  `[roles."surface.user"]` background, a band, or the terminal canvas) while the
  chevron keeps its prompt colour. Unset means the model-colour wash. `Cards`
  and `Still` set it to `false`.

Unknown background profiles are always unpainted, so `prompt_wash` never invents
a fill where the terminal could be light, dark, or custom.

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
appearance; the welcome is static at that boundary. SSH alone does not
reduce the advertised terminal color capability.

Auto can issue one OSC 11 background query and use a neutral fallback after its
short detection deadline. The shared input owner continues recognizing a late
reply after that deadline, including slowly fragmented bodies once the OSC 11
header is recognized. Genuine typing and bracketed paste are retained.

Escape and Alt+] remain genuine keys: an incomplete opening header has a 250 ms
ambiguity timeout. A header fragmented more slowly can still pass through as
input. Use explicit `--theme dark` or `--theme light` to skip the query on such
terminals. This is not a guarantee for arbitrary terminal-protocol corruption.

Native Windows never sends the query. The console host turns a reply into key
records, and Windows Terminal's reply reached the composer as typed text, so
Auto uses the neutral fallback there. Choose `--theme dark`, `--theme light` or
`/theme` to pick a contrast profile.

## Variant reference

octet ships a complete reference file for the compiled default theme at
[`examples/themes/octet-default.toml`](../examples/themes/octet-default.toml).
It is not read at runtime — the compiled default is always the fallback — but it
documents the accepted sections in one place and is a valid starting point:

```console
cp examples/themes/octet-default.toml ~/.octet/themes/mine.toml
```

`examples/themes/Cards.toml` is compiled into every release as the built-in
`Cards` selector, so it is available without copying anything:

```console
octet --theme Cards
OCTET_THEME=Cards octet
```

`Cards` is a shaded-surfaces theme — flush-left transcript rails and prose
with one-cell content padding inside the rails, dark/light fills, and
historical prompts' model-coloured rails and chevrons (rather than amber
rails), a quiet amber UI accent, a borderless shaded composer, and a themed
compact splash. Its slash-command choices align with the shaded composer's
chevron and input text. The `prompt_rail_model_adaptive = true` token opts a
file theme's user rails into each prompt's stored model colour, falling back
to its historical model family (or the active accent if none is known). Other surface border roles continue to control their own rail colours.
It is offered in `/theme` alongside the terminal-appearance choices, and its
`Cards` stem is reserved so a local `Cards.toml` can neither shadow nor be
shadowed by the built-in. Editing
[`examples/themes/Cards.toml`](../examples/themes/Cards.toml) changes the
built-in; `cards_example_theme_is_valid_for_every_background_profile` in
`crates/octet-coding-agent/src/tui/theme.rs` fails the build if it stops
validating for any terminal-background profile.

`examples/themes/Still.toml` is compiled into every release the same way, as the
built-in `Still` selector:

```console
octet --theme Still
OCTET_THEME=Still octet
```

`Still` uses the available terminal width for its transcript and softly shaded
composer. A quiet band distinguishes user prompts. One extra breathing cell
before transcript markers aligns prompt, activity, and prose text. Prose and
tool activity remain unboxed, with compact adjacent rows and monochrome dots.
Consecutive reads, searches, and commands share an exploration summary;
consecutive edits and writes show a distinct-file count. Web searches and fetches,
MCP calls, and computer-use actions each get their own quiet summary; delegation
keeps its existing subagent presentation. `Ctrl+O` reveals individual paths,
commands, calls, and failures. Its
`model.use_lab_color = "true"` token keeps the prompt chevron and composer
marker model-adaptive, and `splash_model_adaptive` with `splash_compact` give
it the compact 16x4 startup mark shaded from the active model family. The dark
UI uses a restrained blue accent over neutral grays. Reasoning is hidden by
default.
The reserved row under its live `Working` indicator prevents the composer from
bouncing as two-line `Thinking` takes over. While tools run, including collapsed
commands, the `Working` indicator remains below their activity until the run
settles. This liveness behavior also applies to the default theme. It is
offered in `/theme` after `Cards`, its `Still` stem is reserved, and
`still_example_theme_is_valid_for_every_background_profile` in
`crates/octet-coding-agent/src/tui/theme.rs` validates the embedded file for
every terminal-background profile.

The startup welcome card follows the theme's content width and leading inset,
just like the transcript, composer, and pickers. Width-capped custom themes
center the splash; uncapped themes such as `Still` use the available width.
The welcome fits the actual pane height after reserving the composer and gap.
Short panes use a compact identity and permission disclosure instead of sending
an oversized decorative card into native history. Late update hints replace
existing hint rows rather than moving the composer.

Three optional top-level tokens shape the startup splash:

- `splash` — colour for the byte-mark and splash text. On a truecolor terminal
  it uses a static column gradient.
- `splash_compact` — `true` selects the default's smaller geometry (4-tall
  mark at 16 columns) instead of the larger file-theme presentation.
- `splash_model_adaptive` — `true` keeps the default's model-adaptive
  splash: the byte-mark follows the active model family and the splash text
  uses the model accent. It claims the whole splash, so `splash` is not used.
  `Cards` uses it.

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
