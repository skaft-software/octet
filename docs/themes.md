# octet themes

octet's compiled default theme keeps model-family accents, adapts to your
terminal background and uses semantic status colors. Pick a theme with `/theme`,
`--theme NAME` or `OCTET_THEME`.

## Choose a theme

At first run, the appearance picker offers:

- **Auto (recommended)** detects the terminal background from reliable
  environment or terminal-capability signals, and uses a readable neutral
  fallback when detection isn't possible.
- **Light terminal** and **Dark terminal** pick the matching contrast profile
  and override detection.

Moving through the picker previews each appearance without saving it. Confirming
saves `theme = "auto"`, `"light"` or `"dark"` in your user config without
replacing other settings. Dismissing onboarding uses Auto. Existing configured
installs don't reopen onboarding, and print, plain and RPC sessions, redirected
output and `TERM=dumb` never open it.

Use `/theme` later to browse the built-in choices **and** valid `.toml` files
from normal resource discovery: global `~/.octet/themes`, a trusted project's
`.octet/themes`, and directories or files passed with `--theme-dir`. The list
shows each file stem and its optional metadata name and description, and typing
filters it. Selecting a file previews it, and confirming saves its file stem
(for example `theme = "mine"`) in your user config, leaving other settings
intact. `/theme mine` (or `/theme mine.toml`) picks a discovered, valid file
directly, and `/theme Cards` and `/theme Still` pick the built-in file themes
described below. Cancelling restores the active theme without touching your
config or session. The first-run picker still shows only Auto, Light and Dark.

Discovery goes global, then trusted project, then explicit path, and the later
one wins. When two files share a stem, only the winner shows, even if it's
invalid. Files that fail bounded reads or schema validation are left out of the
list. The stems `auto`, `light`, `dark`, `default`, `Cards` and `Still` are
reserved for built-in selectors and never show up as file choices.

`--theme` and `OCTET_THEME` take the built-in choices, or a named file at
startup. There's no theme marketplace. An unrecognized or malformed name at
startup falls back to the compiled default. An explicit `OCTET_COLOR_SCHEME`
also counts as an existing terminal-appearance choice, so automation and
already-configured shells aren't interrupted by onboarding.

<details>
<summary>What the default theme does</summary>

Auto, Dark and Light share the same default geometry: a 16×4 model-blended
startup mark, a full-cell model-adaptive card for each historical prompt (the
stored color of the model that received it fills the whole cell, cushion rows
and inline Markdown styling included), model-colored composer rules, one
blinking tool-like subagent transcript row while workers run, two-row command
previews, and full available prose width without narrowing code or tables. The
footer may omit a redundant catalog-owned provider prefix, never a configured
model name. These are compiled presentation policies, not new theme-file fields:
custom surfaces, composer frames and loaded theme colors and geometry keep their
own styling. Unknown-background and no-color terminals keep a readable,
unpainted prompt.

A theme that sets `content_max_width` gets a centered reading column for its
text, but its filled chrome still reaches the terminal edges. A shaded composer
and borderless `band` or `rail` surfaces paint edge to edge instead of floating
as a narrow island with unpainted bars beside them. Bordered cards, plain text,
and themes that only inset their content without capping the column are
unchanged.

</details>

## Prompt provenance

Every submitted prompt stores the exact color of the model that received it, so
a session keeps its provenance after a model switch or a restart. The default
theme turns that stored color into a full-cell card. The background covers the
whole row (marker gutter, padding, trailing canvas, and the breathing rows above
and below), and the rich renderer's bold, italic and inline-code runs are
layered inside the card instead of flattened onto one color. Unknown-background
and no-color terminals leave the prompt unpainted on the terminal canvas.

One top-level token changes that. `prompt_wash = false` keeps prompt rows on the
surface's own fill (a `[roles."surface.user"]` background, a band, or the
terminal canvas) while the chevron keeps its prompt color. Unset means the
model-color wash. `Cards` and `Still` set it to `false`. Unknown background
profiles are always unpainted, so `prompt_wash` never invents a fill where the
terminal could be light, dark or custom.

## Activity status contrast

`Thinking` and `Working` use model-family foreground colors only, and the
shimmer never paints a character background. Set `OCTET_SHIMMER=classic` for the
legacy stepped shimmer, for A/B testing. Classic is also forced for ANSI16,
unknown-background, no-color and reduced-motion profiles, and an unrecognized
value uses the physical default when the terminal supports it.

<details>
<summary>The physical shimmer</summary>

On known Dark and Light TrueColor or ANSI256 profiles, the default physical
shimmer uses a raised-cosine light field with a short leading edge, a longer
trailing tail and a small central glint. Dark labels move from about `0.55`
toward `0.99` luminance, and light labels from about `0.0085` toward `0.11`. The
motion takes roughly 0.85 to 1.0 seconds, then parks for two ticks before
repeating. The margin dot shares that phase without changing its glyph or size.
The qualification fixture checks representative Dark and Light composited
surfaces, and doesn't measure arbitrary transparency. Custom ANSI16 palettes can
also change perceived contrast.

</details>

## Startup and terminal replies

The first branded frame waits for resolved model and setup state, workspace and
appearance, and the welcome animation starts at that boundary. SSH alone doesn't
reduce the advertised terminal color capability. Native Windows never sends the
background query: the console host turns a reply into key records, and Windows
Terminal's reply reached the composer as typed text. Auto uses the neutral
fallback there, so pick `--theme dark`, `--theme light` or `/theme` for a
contrast profile.

<details>
<summary>How the OSC 11 background query behaves</summary>

Auto can issue one OSC 11 background query and use a neutral fallback after its
short detection deadline. The shared input owner keeps recognizing a late reply
after that deadline, including slowly fragmented bodies once the OSC 11 header
is recognized. Genuine typing and bracketed paste are kept. Escape and Alt+]
stay genuine keys: an incomplete opening header has a 250 ms ambiguity timeout,
and a header fragmented more slowly can still pass through as input. Use
explicit `--theme dark` or `--theme light` to skip the query on such terminals.
This isn't a guarantee for arbitrary terminal-protocol corruption.

</details>

## Variant reference

octet ships a complete reference file for the compiled default theme at
[`examples/themes/octet-default.toml`](../examples/themes/octet-default.toml).
It isn't read at runtime, because the compiled default is always the fallback,
but it documents the accepted sections in one place and is a valid starting
point:

```console
cp examples/themes/octet-default.toml ~/.octet/themes/mine.toml
```

### Cards

`examples/themes/Cards.toml` is compiled into every release as the built-in
`Cards` selector, so it's available without copying anything:

```console
octet --theme Cards
OCTET_THEME=Cards octet
```

`Cards` is a shaded-surfaces theme: flush-left transcript rails and prose with
one-cell content padding inside the rails, dark and light fills, historical
prompts' model-colored rails and chevrons (rather than amber rails), a quiet
amber UI accent, a borderless shaded composer and a themed compact splash. Its
slash-command choices align with the shaded composer's chevron and input text.
It's offered in `/theme` alongside the terminal-appearance choices.

<details>
<summary>Cards rails, reserved stem and validation</summary>

The `prompt_rail_model_adaptive = true` token opts a file theme's user rails
into each prompt's stored model color, falling back to its historical model
family (or the active accent if none is known). Other surface border roles keep
controlling their own rail colors. The `Cards` stem is reserved, so a local
`Cards.toml` can neither shadow nor be shadowed by the built-in. Editing
[`examples/themes/Cards.toml`](../examples/themes/Cards.toml) changes the
built-in, and `cards_example_theme_is_valid_for_every_background_profile` in
`crates/octet-coding-agent/src/tui/theme.rs` fails the build if it stops
validating for any terminal-background profile.

</details>

### Still

`examples/themes/Still.toml` is compiled in the same way, as the built-in
`Still` selector:

```console
octet --theme Still
OCTET_THEME=Still octet
```

`Still` uses the available terminal width for its transcript and softly shaded
composer. A quiet band distinguishes user prompts, and one extra breathing cell
before transcript markers aligns prompt, activity and prose text. Prose and tool
activity stay unboxed, with compact adjacent rows and monochrome dots.
Consecutive reads, searches and commands share an exploration summary, and
consecutive edits and writes show a distinct-file count. Web searches and
fetches, MCP calls and computer-use actions each get their own quiet summary,
and delegation keeps its existing subagent presentation. Ctrl+O reveals
individual paths, commands, calls and failures. The dark UI uses a restrained
blue accent over neutral grays, and reasoning is hidden by default. It's offered
in `/theme` after `Cards`.

<details>
<summary>Still tokens, liveness and validation</summary>

Its `model.use_lab_color = "true"` token keeps the prompt chevron and composer
marker model-adaptive, and `splash_model_adaptive` with `splash_compact` give it
the compact 16x4 startup mark shaded from the active model family. The reserved
row under its live `Working` indicator keeps the composer from bouncing as
two-line `Thinking` takes over. While tools run, including collapsed commands,
the `Working` indicator stays below their activity until the run settles. This
liveness behavior also applies to the default theme. The `Still` stem is
reserved, and `still_example_theme_is_valid_for_every_background_profile` in
`crates/octet-coding-agent/src/tui/theme.rs` validates the embedded file for
every terminal-background profile.

</details>

The startup welcome card follows the theme's content width and leading inset,
like the transcript, composer and pickers. Width-capped custom themes center the
splash, and uncapped themes such as `Still` use the available width. Three
optional top-level tokens shape it:

- `splash`: the color for the byte-mark and splash text. With a truecolor,
  animation-capable terminal it shades into a column gradient.
- `splash_compact`: `true` selects the default's smaller geometry (a 4-tall mark
  at 16 columns) instead of the larger file-theme presentation.
- `splash_model_adaptive`: `true` keeps the default's model-adaptive splash. The
  byte-mark follows the active model family and the splash text uses the model
  accent. It claims the whole splash, so `splash` isn't used. `Cards` uses it.

A theme file is a bounded TOML document (256 KiB) with these typed sections:

- `[metadata]`: `name`, `description`, `author`, `version`, `terminal`
  (`light-dark`, `dark`, `light` or `any`), and optional `adaptive` to rebalance
  RGB foregrounds and surfaces for the detected terminal background.
- `[colors]` / `[tokens]`: flat or nested color and token values. `"default"`
  means the terminal's own color.
- `[roles.<name>]`: per-role `foreground`, `background`, `bold`, `dim`,
  `italic`, `underline`, `strikethrough`, `inverse` and `adaptive`.
- `[glyphs]` / `[glyphs_ascii]`: typed glyphs. Every structural glyph is one
  column, and ASCII fallbacks must be ASCII.
- `[surfaces.<kind>]`: bounded layout recipes for the `user`, `assistant`,
  `reasoning`, `tool`, `notice`, `outcome`, `shell` and `compaction` transcript
  surfaces.
- `[layout]`: density, shell visibility and narrow-terminal overrides.
- `[variants.*]`: background overlays, described next.

`[variants.universal]`, `[variants.dark]` and `[variants.light]` merge
recursively over the base document using the same section shapes. `universal`
applies to every terminal, then `dark` or `light` overlays it for the detected
background profile, so one variant can override a single token without restating
the whole table. `[variants.unknown]` applies when detection fails.

Theme files reject terminal control bytes, unknown sections and fields, invalid
role names, non-ASCII ASCII-fallbacks, wide or empty structural glyphs and
oversized values. Unknown or partial files fall back to the compiled default
rather than starting with a broken shell.

## Semantic role vocabulary

`[roles.<name>]` is typed. These names are the published, terminal-independent
vocabulary that maps onto octet's semantic text roles. The
`published_semantic_role_vocabulary_is_closed_and_accepted` test in
`crates/octet-coding-agent/src/tui/theme.rs` keeps this list and
`SEMANTIC_ROLE_VOCABULARY` in sync. Alias spellings (`foreground` and `text`,
`dim` and `subtle`, `code` and `md_code_block`, `diff_add` and `diff_added`, and
so on) resolve to the same underlying role.

<details>
<summary>The role names</summary>

`text`, `foreground`, `muted`, `subtle`, `dim`, `accent`, `success`, `warning`,
`error`, `heading`, `md_heading`, `emphasis`, `md_emphasis`, `strong`,
`md_strong`, `inline_code`, `md_code`, `code`, `md_code_block`, `quote`,
`md_quote`, `border`, `link`, `md_link`, `list_marker`, `md_list_bullet`,
`diff_add`, `diff_added`, `diff_remove`, `diff_removed`, `diff_context`,
`diff_hunk`, `diff_header`, `syntax_comment`, `syntax_keyword`,
`syntax_function`, `syntax_variable`, `syntax_string`, `syntax_number`,
`syntax_type`, `syntax_operator`, `syntax_punctuation`.

</details>

## Extension-contributed themes and roles

Two channels are open to extensions:

- **Roles.** Any name of the form `extension.<namespace>.<role>` is accepted in
  `[roles]` (up to 96 bytes, ASCII alphanumerics plus `_`, `-` and `.`). The
  schema is open but typed: an extension may add
  `[roles."extension.git.branch"]` without a host change, but can't inject the
  same name under a private, unnamespaced key.
  `semantic_extension_roles_are_open_but_typed` in `theme_schema.rs` tests this.
- **Theme files.** An extension package may ship `themes/<name>.toml`. Install
  or copy it into a discovery root so the shared resolver can select it:
  `~/.octet/themes/` (global), `.octet/themes/` (project, needs trust), or a
  directory passed with `--theme-dir`. Discovered files use the same bounded,
  no-follow reader as the compiled default and never run extension code.

A manifest-level `contributes.themes` channel that would register an extension's
own directory as a theme root is **not implemented**. It needs a change in the
extension manifest schema and discovery, outside the theme module. Until then,
publishing a theme file into a discovery root is the supported way to contribute
one.
