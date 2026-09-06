# octet themes

Theme customization is disabled in octet v0.7.0. The terminal and graphical Serve
frontend always use octet's compiled default theme. The default remains
model-aware: deterministic model-family palettes color active completion and
picker controls consistently without changing layout, interaction grammar, or
semantic status colours.

The `/theme` command is not registered, and the runtime does not discover or
load bundled or filesystem theme files. Legacy `--theme`, `--theme-dir`,
`theme` configuration, and `OCTET_THEME` inputs are retained only for launch
compatibility: they are ignored or fall back to the compiled default.

The typed theme implementation and schema remain in the source tree so theme
support can be restored in a later release. They are not part of the v0.7.0
user-facing customization contract.
