# Theme examples

- [`octet-default.toml`](octet-default.toml) is the shipped variant reference for
  octet's compiled-in `default` theme (roadmap #416). It is a complete,
  schema-valid theme file that demonstrates `[variants.universal]`,
  `[variants.dark]`, and `[variants.light]`, the published semantic role
  vocabulary, the `extension.<namespace>.<role>` contribution channel, typed
  glyphs with ASCII fallbacks, transcript surfaces, and layout.
- [`Cards.toml`](Cards.toml) (`Cards`) is a ready-to-use shaded-surfaces
  theme: compact rail chrome with model-coloured prompt stripes, dark/light
  fills, a quiet amber UI accent, a borderless shaded composer, and a themed
  splash with compact geometry.
- [`Still.toml`](Still.toml) (`Still`) is a calm, full-width theme: soft user and
  composer fills, quiet activity summaries for exploration, edits, web, MCP, and
  computer use, plus model-adaptive prompt chevrons.

`Cards` and `Still` are compiled into every release, so they need no copy. They
appear in `/theme` after the terminal-appearance choices and work with
`--theme Cards`, `--theme Still`, and `OCTET_THEME`. Their file stems are
reserved: a local `Cards.toml` or `Still.toml` can neither shadow, nor be
shadowed by, the built-in it selects.

`octet-default.toml` is a reference, not the compiled fallback. octet never
reads this file at runtime; unknown or partial theme files always fall back to
the compiled default. Copy it into a discovery directory to start your own
theme:

```console
mkdir -p ~/.octet/themes
cp examples/themes/octet-default.toml ~/.octet/themes/mine.toml
```

Then name it at startup with `--theme mine` or `OCTET_THEME=mine`. The
first-run appearance picker shows only the three terminal appearances (`auto`,
`light`, `dark`); the full list of built-ins is available later through
`/theme`. Theme files are bounded (256 KiB) and
reject control characters, unsafe glyphs, and unknown sections. See
[docs/themes.md](../../docs/themes.md) for the full contract and the published
role vocabulary.
