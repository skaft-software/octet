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

From the repository root, using your existing Cargo target directory:

```sh
export CARGO_TARGET_DIR=/absolute/existing-target
CARGO_PROFILE_DEV_DEBUG=0 cargo build -p octet-coding-agent --example native-editor-test-host --locked -j3
npm run test:editor-native --prefix extensions/octet-pi-compat
npm test --prefix extensions/octet-pi-compat
```

Alternatively set `OCTET_NATIVE_EDITOR_TEST_HOST` to the built example binary.
Missing native peers fail explicitly: there is no JavaScript editing stub or Pi
engine fallback. The peer compiles the production `native_editor.rs` service and
owns each fixture's real sexy-tui-rs `TextEditor` models through synchronous,
bounded JSON pipes. Only its owned child is closed during test cleanup.

Both upstream suites remain included in ordinary `npm test`.
`npm run test:editor-native` additionally runs the unchanged ownership acceptance
as a distinct gate. Each upstream suite runs in its own Node test process, as
upstream does, so history's global keybinding reset cannot leak to another file.
Jiti uses the already-pinned adapter dependency, with disk caching disabled.
No external reference checkout, extra dependencies or network access is needed.

## Subject and fixtures

`support.mjs` changes module resolution, not test bodies. `subject.mjs` imports
Editor from the actual adapter `shims/tui.mjs` export used by extension factories.
The loader checks that identity; it cannot silently substitute a reference Editor.
`CustomEditor` inherits this same native-backed facade, not Pi's editing class.
The upstream `wordWrapLine` assertions separately exercise Pi's pure wrapping
helper, retained in the extension library role. They are **not evidence about
native wrapping, editor state, or caret ownership**.

Only the terminal/theme fixtures change:

- Upstream's unstarted `TuiMainScreen`/xterm fixture becomes the adapter's real
  `RemoteTUI` facade with the same columns and rows. These suites never start a
  terminal or inspect its viewport. Its test-only binding delegates every editor
  operation to the production Rust service; it implements no editing, state,
  input handling, undo, history, paste registry or autocomplete.
- The Editor test theme uses upstream Chalk level-3 SGR pairs directly, avoiding
  an additional Chalk dependency. The suites still perform all original ANSI,
  visible-width, cursor, border and autocomplete assertions.
- Provider callback APIs, keybindings and width utilities use the pinned Pi
  library. Native query/revision policy, menu selection and completion mutations
  remain Rust-owned; JS retains provider callback handles and presentation only.

## Qualification and deliberate bounds

The native run passes **194/194, zero failures/skips/cancellations**: all 193
immutable upstream cases plus the independent ownership guard. Earlier Pi-backed
phase-1 baseline and native red receipts remain historical evidence, not current
native failures or proof of native ownership.

The headless peer qualifies native-service/facade behavior, **not frontend
admission or terminal painting**. Separate actual App/actual adapter tests cover
Unicode caret/undo/submit callbacks, exact owner/mount/live-surface admission,
one native registry argument menu using the custom editor's actual caret,
suffix-preserving keyboard selection/acceptance, and rejection of late callbacks
after native clear or retirement. The composer-slot PTY separately checks native
chrome, one draft/one popup and coordinated restoration.

There are no remaining behavior gaps in this pinned corpus. Deliberate native
boundary differences remain: 16 editors/surface; 256 KiB raw/expanded draft;
100 history entries/256 KiB aggregate; 64 paste recovery entries/4 MiB; and bounded,
control-free provider results. Completion coordinates must be representable
native grapheme boundaries. Registry argument completion retains its narrower
suffix-only wire profile, including explicit refusal of unrepresentable quote
edits. These are not claims about arbitrary overrides or complete Pi API parity.
