# Pi 1.5.0 helper sources

Pi 1.5.0 is the release this adapter emulates when no installed Pi answers a
module. `UPSTREAM.json` records the reference commit and the sha256 of every
reviewed dist file the shims bind or classify; `../manifests/1.5.0.json` is the
capability manifest that names them.

What this release changed, and how the adapter follows it:

- **Assistant-message frames are gone.** `AssistantMessageFrameEncoder` and
  `reduceAssistantMessageFrames` were removed with Pi's experimental agent
  harness, and `pi-ai/dist/utils/assistant-message-frame.js` no longer exists.
  The shims still export both names because a Pi 1.0.2-1.4 install answers them;
  on 1.5.0 invoking either fails loudly with the symbol and the release named
  (`unsupported_feature pi-ai.<symbol>: Pi 1.5.0 does not ship it (removed in
  1.5.0)`), never a silent `undefined`.
- **pi-ai no longer re-exports TypeBox.** `Type`, `Static` and `TSchema` come from
  `typebox`, and extensions get `Type`/`StringEnum` from the coding agent's
  `pi-ai-extension-view.js`; the shim binds its own `typebox` and its own
  `StringEnum` with the same JSON.
- **Tool arguments are validated with pi-schema.** `ai/validation.mjs` is the
  single port of `packages/ai/src/utils/validation.ts` at 1.5.0: an
  `argumentsError` call fails before schema work, arguments are coerced and then
  checked, and failures use pi-schema's dotted paths and messages (`- limit:
  expected number, got string`, `- edits.0.oldText: missing`). Where a supported
  installed Pi answers, the shim prefers Pi's own `dist/utils/validation.js`.
- **Virtual modules.** `pi-ai`, `pi-ai/compat`, `pi-ai/models`, `pi-ai/oauth`,
  `pi-ai/providers/all`, `pi-coding-agent`, `pi-agent-core` and `pi-tui` are the
  specifiers Pi resolves (both scopes). The route map in the manifest mirrors
  `dist/core/extensions/virtual-modules.js`.

These adapted sources are covered by [Pi's MIT license](../../LICENSE.pi).
