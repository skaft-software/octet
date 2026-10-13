# Octet cursor themes

The bundled `cua.default.lottie` source comes from
[trycua/cua](https://github.com/trycua/cua/blob/11c4647128a99b2879f31a3f4eedc6b08d52c079/libs/cua-driver/rust/crates/cursor-overlay/assets/cua.default.lottie)
(commit `11c4647128a99b2879f31a3f4eedc6b08d52c079`, SHA-256
`322e40a33475599b4403a6cb2ad3286d3312a6f9c71d94abfc92dbca8fa99de8`).
Cua's MIT license is preserved in `CUA-LICENSE.md`.

`build.py` derives the 24 compiled, self-contained Cua v2 theme artifacts:
it retains all twelve semantic action animations, reshapes the pointer after
Octet's rounded-arrow icon, reduces its footprint, and colors each variant
with Octet's stable model-prompt palette (`tui/theme.rs`, unknown terminal
background). The native session badge remains Cua's own session color. Cua's
bounded vector profile does not support the icon's gradient; the fill and glow
use the model color instead. The custom theme also does not inherit Cua's
special built-in idle levitation; the extension configures a short, straight
movement through the driver motion settings.

To regenerate with a Cua Driver version supporting `cua-driver-actions-v2`:

```sh
python3 extensions/octet-computer-use/themes/build.py /path/to/cua-driver
```

The generated `palette.json` and `.cua-theme` files are checked in and shipped
in the extension. A user-initiated `/computer-use setup` installs only these
bundled artifacts through Cua's trusted local CLI, never through an agent tool.
Where the driver ships no theme compiler (the `cua-driver` wheels, used on
Linux), the extension performs the CLI's own install step instead: an atomic
write of `<id>.cua-theme` into Cua's theme store, which the driver validates
again on load. On Linux that also happens the first time the cursor starts.
