# Theme examples

- [`octet-default.toml`](octet-default.toml) is the shipped variant reference for
  octet's compiled-in `default` theme (roadmap #416). It is a complete,
  schema-valid theme file that demonstrates `[variants.universal]`,
  `[variants.dark]`, and `[variants.light]`, the published semantic role
  vocabulary, the `extension.<namespace>.<role>` contribution channel, typed
  glyphs with ASCII fallbacks, transcript surfaces, and layout.

It is a reference, not the compiled fallback. octet never reads this file at
runtime; unknown or partial theme files always fall back to the compiled
default. Copy it into a discovery directory to start your own theme:

```console
mkdir -p ~/.octet/themes
cp examples/themes/octet-default.toml ~/.octet/themes/mine.toml
```

Then name it at startup with `--theme mine` or `OCTET_THEME=mine`. The
interactive `/theme` command still selects only the three built-in terminal
appearances (`auto`, `light`, `dark`). Theme files are bounded (256 KiB) and
reject control characters, unsafe glyphs, and unknown sections. See
[docs/themes.md](../../docs/themes.md) for the full contract and the published
role vocabulary.
