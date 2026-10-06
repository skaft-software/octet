# Pinned Pi 1.0.2 Editor suites

The two `test/*.test.ts` files are verbatim copies of the reviewed sources
supplied at `orders/pi-v1.0.2/packages/tui/test`:

- `editor.test.ts`: 192 cases.
- `editor-history-keybindings.test.ts`: 1 case.

Copyright (c) 2025 Mario Zechner, MIT; see [LICENSE.pi](../../LICENSE.pi).
`provenance.json` records their SHA-256 hashes. The loader verifies those hashes
before registering any cases. No assertions, input sequences, callbacks, timer
advances, or cases were changed, skipped, or marked TODO.

## Run

From `extensions/octet-pi-compat`:

```sh
npm run test:editor-native
npm test
```

Both upstream suites remain included in ordinary `npm test` as phase-1 baseline
coverage. `npm run test:editor-native` additionally runs the unchanged ownership
acceptance as the explicit phase-2 qualification gate. It must be green before
claiming native parity; phase 1 deliberately still uses Pi's Editor component.
No external reference checkout, extra dependencies, network access, or environment
variable is needed. Each upstream suite runs in its own Node test process, as
upstream does, so history's global keybinding reset cannot leak to another file.
Jiti uses the already-pinned adapter dependency, with disk caching disabled.

## Subject and fixtures

`support.mjs` changes module resolution, not test bodies. `subject.mjs` imports
Editor from the actual adapter `shims/tui.mjs` export used by extension factories.
The loader checks that identity; it cannot silently substitute a reference Editor.
The upstream `wordWrapLine` assertions separately exercise Pi's pure wrapping
helper, retained in the extension library role. They are **not evidence about
native wrapping, editor state, or caret ownership**.

Only the terminal/theme fixtures change:

- Upstream's unstarted `TuiMainScreen`/xterm fixture becomes the adapter's real
  `RemoteTUI` facade with the same columns and rows. These suites never start a
  terminal or inspect its viewport. The fixture implements no editing, state,
  input handling, undo, history, paste registry, or autocomplete.
- The Editor test theme uses upstream Chalk level-3 SGR pairs directly, avoiding
  an additional Chalk dependency. The suites still perform all original ANSI,
  visible-width, cursor, border, and autocomplete assertions.
- Autocomplete, keybindings, and width utilities use the pinned Pi library.

## Current result and phase-2 gaps

On this candidate, the 193 upstream cases pass with zero skips. This is only a
**Pi-backed adapter baseline**: the public Editor export still is Pi's JavaScript
Editor, and CustomEditor still inherits it. The mandatory ownership test is red
with `native-backed facade missing: the adapter still exports Pi Editor directly`.
Consequently `npm run test:editor-native` is **not green**, and no native Editor
parity is qualified. The phase-1 `npm test` result is reported independently.

Native-backed execution still requires the production Editor facade and its
owner-fenced synchronous native editor-operation service. Once that seam exists,
the geometry fixture must bind to the real native service, not a JavaScript editor
stub. The exports/ownership test must pass without changing the upstream cases.
Facade-backed native state/caret/undo/paste/history/key handling/autocomplete/
submit and the custom-editor native registry argument-completion seam remain
unimplemented, rather than individual upstream cases being silently excluded.
